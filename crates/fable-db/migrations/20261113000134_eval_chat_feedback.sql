-- FAB-34: retrieval evaluation and chat feedback (spec §7 M-4; §12.3 E-1..E-6).
--
-- The M-5 filter indexes (documents.source_updated_at / mime_type /
-- source_id) and query_logs.filters (F-6) were created with the core tables
-- (20261113000129_core_tables.sql).
--
-- Status/rating columns are TEXT backed by Rust enums (M-6), never Postgres
-- enums. Constraint names follow the Python naming convention
-- (pk_/uq_/fk_/ck_/ix_).
--
-- Collection references: eval sets and chat feedback span several
-- collections, so they hold `collection_ids uuid[]` (E-1) instead of a
-- foreign key. The `collections_detach_eval_feedback` trigger keeps them
-- consistent with ST-2 ("derived rows cascade from collections"): deleting a
-- collection removes its id from those arrays, and a set or feedback row left
-- with no collection is deleted (cascading to items, runs and results).
--
-- Document and chunk ids inside eval items/results and feedback are not
-- foreign keys: a golden item whose expected document disappeared must keep
-- failing (that is a regression signal), and results must survive reindexing,
-- which replaces chunk ids.

-- ---------------------------------------------------------------------------
-- eval_sets (E-1): a named golden set over one or more collections.
-- ---------------------------------------------------------------------------
CREATE TABLE eval_sets (
    id             uuid         NOT NULL DEFAULT gen_random_uuid(),
    name           varchar(200) NOT NULL,
    description    text         NOT NULL DEFAULT '',
    collection_ids uuid[]       NOT NULL,
    created_by     varchar(100) NOT NULL DEFAULT '',
    created_at     timestamptz  NOT NULL DEFAULT now(),
    updated_at     timestamptz  NOT NULL DEFAULT now(),

    CONSTRAINT pk_eval_sets PRIMARY KEY (id),
    -- `fable eval --set <name>` (E-3) resolves sets by name.
    CONSTRAINT uq_eval_sets_name UNIQUE (name),
    CONSTRAINT ck_eval_sets_name_not_blank CHECK (btrim(name) <> ''),
    CONSTRAINT ck_eval_sets_collection_ids_not_empty
        CHECK (cardinality(collection_ids) > 0 AND array_position(collection_ids, NULL) IS NULL)
);

-- "Evaluation" page per collection: sets WHERE collection_ids @> ARRAY[$1].
CREATE INDEX ix_eval_sets_collection_ids ON eval_sets USING gin (collection_ids);

-- ---------------------------------------------------------------------------
-- eval_items (E-1, E-2): one golden question with its expected documents
-- (and optionally chunks). `origin` records how it was seeded.
-- ---------------------------------------------------------------------------
CREATE TABLE eval_items (
    id                    uuid        NOT NULL DEFAULT gen_random_uuid(),
    set_id                uuid        NOT NULL,
    question              text        NOT NULL,
    expected_document_ids uuid[]      NOT NULL DEFAULT '{}',
    -- Optional finer-grained expectation; NULL when not specified.
    expected_chunk_ids    uuid[],
    notes                 text        NOT NULL DEFAULT '',
    -- manual | query_log | feedback | import
    origin                varchar(20) NOT NULL DEFAULT 'manual',
    -- The chat_feedback row a thumbs-down item was created from (E-6).
    -- Not a foreign key: the item outlives the feedback.
    origin_feedback_id    uuid,
    created_by            varchar(100) NOT NULL DEFAULT '',
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT pk_eval_items PRIMARY KEY (id),
    CONSTRAINT fk_eval_items_set_id_eval_sets
        FOREIGN KEY (set_id) REFERENCES eval_sets (id) ON DELETE CASCADE,
    CONSTRAINT ck_eval_items_question_not_blank CHECK (btrim(question) <> ''),
    -- Same cap as query_logs.query (R-7).
    CONSTRAINT ck_eval_items_question_length CHECK (char_length(question) <= 4000),
    CONSTRAINT ck_eval_items_origin_valid
        CHECK (origin IN ('manual', 'query_log', 'feedback', 'import'))
);

-- Items of a set, in creation order (keyset paging, UX-8).
CREATE INDEX ix_eval_items_set_id_created_at ON eval_items (set_id, created_at, id);

-- ---------------------------------------------------------------------------
-- eval_runs (E-3, E-4): one evaluation of a set. Runs triggered from the API
-- or portal are queued as jobs; the CLI writes a row directly.
-- ---------------------------------------------------------------------------
CREATE TABLE eval_runs (
    id              uuid         NOT NULL DEFAULT gen_random_uuid(),
    set_id          uuid         NOT NULL,
    -- queued | running | succeeded | failed
    status          varchar(10)  NOT NULL DEFAULT 'queued',
    -- Principal whose ACL the run searched under (E-3).
    requested_by    varchar(100) NOT NULL DEFAULT '',
    -- Pipeline settings, embedding space, contextual on/off, mode, k values,
    -- filters (E-4).
    config_snapshot jsonb        NOT NULL DEFAULT '{}'::jsonb,
    -- recall@k, MRR, item counts (E-4). Empty until the run finishes.
    metrics         jsonb        NOT NULL DEFAULT '{}'::jsonb,
    error           text         NOT NULL DEFAULT '',
    created_at      timestamptz  NOT NULL DEFAULT now(),
    started_at      timestamptz,
    finished_at     timestamptz,

    CONSTRAINT pk_eval_runs PRIMARY KEY (id),
    CONSTRAINT fk_eval_runs_set_id_eval_sets
        FOREIGN KEY (set_id) REFERENCES eval_sets (id) ON DELETE CASCADE,
    CONSTRAINT ck_eval_runs_status_valid
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed')),
    CONSTRAINT ck_eval_runs_config_snapshot_object
        CHECK (jsonb_typeof(config_snapshot) = 'object'),
    CONSTRAINT ck_eval_runs_metrics_object CHECK (jsonb_typeof(metrics) = 'object'),
    CONSTRAINT ck_eval_runs_started_at
        CHECK ((status = 'queued') = (started_at IS NULL)),
    CONSTRAINT ck_eval_runs_finished_at
        CHECK ((status IN ('succeeded', 'failed')) = (finished_at IS NOT NULL))
);

-- Run history per set, newest first (E-5; keyset paging on (created_at, id)).
CREATE INDEX ix_eval_runs_set_id_created_at ON eval_runs (set_id, created_at DESC, id DESC);

-- ---------------------------------------------------------------------------
-- eval_results (E-3, E-4): per-question outcome of a run.
-- ---------------------------------------------------------------------------
CREATE TABLE eval_results (
    run_id                 uuid    NOT NULL,
    item_id                uuid    NOT NULL,
    -- 1-based rank of the first hit whose document is expected; NULL when no
    -- expected document was retrieved (contributes 0 to MRR).
    rank_of_first_expected integer,
    -- Document id of each hit, in rank order (may repeat: hits are chunks).
    hit_ids                uuid[]  NOT NULL DEFAULT '{}',
    -- Chunk id of each hit, same order as hit_ids.
    hit_chunk_ids          uuid[]  NOT NULL DEFAULT '{}',
    -- Hit scores, same order as hit_ids.
    hit_scores             real[]  NOT NULL DEFAULT '{}',
    latency_ms             integer NOT NULL DEFAULT 0,

    CONSTRAINT pk_eval_results PRIMARY KEY (run_id, item_id),
    CONSTRAINT fk_eval_results_run_id_eval_runs
        FOREIGN KEY (run_id) REFERENCES eval_runs (id) ON DELETE CASCADE,
    CONSTRAINT fk_eval_results_item_id_eval_items
        FOREIGN KEY (item_id) REFERENCES eval_items (id) ON DELETE CASCADE,
    CONSTRAINT ck_eval_results_rank_positive
        CHECK (rank_of_first_expected IS NULL OR rank_of_first_expected >= 1),
    CONSTRAINT ck_eval_results_rank_within_hits
        CHECK (rank_of_first_expected IS NULL
               OR rank_of_first_expected <= cardinality(hit_ids)),
    CONSTRAINT ck_eval_results_hit_arrays_aligned
        CHECK (cardinality(hit_chunk_ids) = cardinality(hit_ids)
               AND cardinality(hit_scores) = cardinality(hit_ids))
);

-- Deleting an item cascades through this FK.
CREATE INDEX ix_eval_results_item_id ON eval_results (item_id);

-- ---------------------------------------------------------------------------
-- chat_feedback (E-6): thumbs up/down (+ comment) on a chat answer. One row
-- per (answer, principal); a second vote updates it.
-- ---------------------------------------------------------------------------
CREATE TABLE chat_feedback (
    id                  uuid         NOT NULL DEFAULT gen_random_uuid(),
    -- The /chat query_logs row (R-7). Feedback goes with its question.
    query_log_id        uuid         NOT NULL,
    principal_oid       varchar(100) NOT NULL,
    principal_name      varchar(200) NOT NULL DEFAULT '',
    -- up | down
    rating              varchar(4)   NOT NULL,
    comment             text         NOT NULL DEFAULT '',
    -- Collections searched by the chat (copied from the query log), so
    -- GET /collections/{id}/feedback filters with an index and owners/editors
    -- only see feedback on their collections.
    collection_ids      uuid[]       NOT NULL,
    retrieved_chunk_ids uuid[]       NOT NULL DEFAULT '{}',
    answer_text         text         NOT NULL DEFAULT '',
    created_at          timestamptz  NOT NULL DEFAULT now(),
    updated_at          timestamptz  NOT NULL DEFAULT now(),

    CONSTRAINT pk_chat_feedback PRIMARY KEY (id),
    CONSTRAINT fk_chat_feedback_query_log_id_query_logs
        FOREIGN KEY (query_log_id) REFERENCES query_logs (id) ON DELETE CASCADE,
    CONSTRAINT uq_chat_feedback_query_log_id_principal_oid
        UNIQUE (query_log_id, principal_oid),
    CONSTRAINT ck_chat_feedback_rating_valid CHECK (rating IN ('up', 'down')),
    CONSTRAINT ck_chat_feedback_comment_length CHECK (char_length(comment) <= 4000),
    CONSTRAINT ck_chat_feedback_collection_ids_not_empty
        CHECK (cardinality(collection_ids) > 0 AND array_position(collection_ids, NULL) IS NULL)
);

-- Per-collection feedback list (E-6): collection_ids @> ARRAY[$1].
CREATE INDEX ix_chat_feedback_collection_ids ON chat_feedback USING gin (collection_ids);
-- Platform-wide list for admins and thumbs-down seeding (E-2), newest first.
CREATE INDEX ix_chat_feedback_rating_created_at
    ON chat_feedback (rating, created_at DESC, id DESC);
CREATE INDEX ix_chat_feedback_created_at ON chat_feedback (created_at DESC, id DESC);

-- ---------------------------------------------------------------------------
-- Collection deletion (ST-2): detach eval sets and feedback from the deleted
-- collection; drop those that no longer belong to any collection.
-- ---------------------------------------------------------------------------
CREATE FUNCTION collections_detach_eval_feedback() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM eval_sets
     WHERE collection_ids <@ ARRAY[OLD.id];
    UPDATE eval_sets
       SET collection_ids = array_remove(collection_ids, OLD.id)
     WHERE collection_ids @> ARRAY[OLD.id];

    DELETE FROM chat_feedback
     WHERE collection_ids <@ ARRAY[OLD.id];
    UPDATE chat_feedback
       SET collection_ids = array_remove(collection_ids, OLD.id)
     WHERE collection_ids @> ARRAY[OLD.id];

    RETURN OLD;
END;
$$;

CREATE TRIGGER collections_detach_eval_feedback
    AFTER DELETE ON collections
    FOR EACH ROW EXECUTE FUNCTION collections_detach_eval_feedback();
