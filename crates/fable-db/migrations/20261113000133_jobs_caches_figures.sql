-- FAB-33: jobs, content-addressed caches and figures (spec §6 ST-2, ST-3;
-- §7 M-4, M-6; §9 Q-1..Q-9; §10.2 I-4..I-6; §10.6 CR-4; R-8).
--
-- Status columns are TEXT backed by Rust enums (M-6), never Postgres enums.
-- Constraint names follow the Python naming convention (pk_/uq_/fk_/ck_/ix_).

-- pgvector: embedding_cache stores vectors. Untyped `vector` (no fixed
-- dimension) because one table serves every embedding space (M-3).
CREATE EXTENSION IF NOT EXISTS vector;

-- ---------------------------------------------------------------------------
-- jobs: the queue that replaces Procrastinate (Q-1).
--
-- Claim (Q-2): SELECT ... WHERE status = 'todo' AND queue = $1 AND run_at <=
-- now() AND (pinned_node IS NULL OR pinned_node = $node) ORDER BY priority,
-- run_at FOR UPDATE SKIP LOCKED. Lower `priority` runs first.
-- ---------------------------------------------------------------------------
CREATE TABLE jobs (
    id           bigint       GENERATED ALWAYS AS IDENTITY,
    queue        text         NOT NULL DEFAULT 'default',
    -- Task name, e.g. `sync`, `index`, `reembed`, `maintenance`.
    task         text         NOT NULL,
    args         jsonb        NOT NULL DEFAULT '{}'::jsonb,
    -- Lower runs first; uploads enqueue with a lower value (U-1).
    priority     integer      NOT NULL DEFAULT 100,
    -- todo | doing | succeeded | failed | cancelled
    status       text         NOT NULL DEFAULT 'todo',
    -- Attempts consumed so far. Rate-limited reschedules don't count (Q-5).
    attempts     integer      NOT NULL DEFAULT 0,
    max_attempts integer      NOT NULL DEFAULT 1,
    -- Not before this time (backoff, scheduled runs).
    run_at       timestamptz  NOT NULL DEFAULT now(),
    -- Worker id (node + worker) holding the job while `doing`.
    locked_by    text,
    locked_at    timestamptz,
    -- Refreshed by the worker while `doing`; stale ones are reset (Q-6).
    heartbeat_at timestamptz,
    -- Queueing lock (Q-3), e.g. `sync:{collection}`, `index:{document}`,
    -- `reembed:{space}`. At most one todo/doing job per key.
    lock_key     text,
    -- Node whose scratch dir holds the input (staged uploads, U-1). Only
    -- that node's workers may claim it; a gone node fails it (Q-6).
    pinned_node  text,
    -- Last error, truncated by the worker (no secrets, no payload bytes).
    last_error   text,
    created_at   timestamptz  NOT NULL DEFAULT now(),
    -- Set when the job reaches a terminal status.
    finished_at  timestamptz,

    CONSTRAINT pk_jobs PRIMARY KEY (id),
    CONSTRAINT ck_jobs_status_valid
        CHECK (status IN ('todo', 'doing', 'succeeded', 'failed', 'cancelled')),
    CONSTRAINT ck_jobs_queue_not_blank CHECK (btrim(queue) <> ''),
    CONSTRAINT ck_jobs_task_not_blank CHECK (btrim(task) <> ''),
    CONSTRAINT ck_jobs_args_object CHECK (jsonb_typeof(args) = 'object'),
    CONSTRAINT ck_jobs_attempts_non_negative CHECK (attempts >= 0),
    CONSTRAINT ck_jobs_max_attempts_positive CHECK (max_attempts >= 1),
    CONSTRAINT ck_jobs_lock_key_not_blank CHECK (lock_key IS NULL OR btrim(lock_key) <> ''),
    CONSTRAINT ck_jobs_pinned_node_not_blank
        CHECK (pinned_node IS NULL OR btrim(pinned_node) <> ''),
    -- A running job always has an owner.
    CONSTRAINT ck_jobs_doing_locked
        CHECK (status <> 'doing' OR (locked_by IS NOT NULL AND locked_at IS NOT NULL)),
    -- finished_at is set exactly for terminal statuses (retention, Q-8).
    CONSTRAINT ck_jobs_finished_at_terminal
        CHECK ((status IN ('succeeded', 'failed', 'cancelled')) = (finished_at IS NOT NULL))
);

-- Queueing lock (Q-3): a duplicate enqueue hits this index and the queue
-- returns AlreadyQueued. Finished jobs release the key.
CREATE UNIQUE INDEX uq_jobs_lock_key
    ON jobs (lock_key)
    WHERE lock_key IS NOT NULL AND status IN ('todo', 'doing');

-- Claim path (Q-2): ready jobs per queue in priority, run_at order.
CREATE INDEX ix_jobs_claim
    ON jobs (queue, priority, run_at, id)
    WHERE status = 'todo';

-- Stale-lock recovery (Q-6).
CREATE INDEX ix_jobs_heartbeat_at
    ON jobs (heartbeat_at)
    WHERE status = 'doing';

-- Pinned jobs of a node: upload-only workers and "node gone" recovery (Q-6).
CREATE INDEX ix_jobs_pinned_node
    ON jobs (pinned_node)
    WHERE pinned_node IS NOT NULL AND status IN ('todo', 'doing');

-- Job retention: delete finished jobs older than 14 days (Q-8).
CREATE INDEX ix_jobs_finished_at
    ON jobs (finished_at)
    WHERE finished_at IS NOT NULL;

-- Admin jobs monitor (UX-8) and /analytics/db status counts (Q-9).
CREATE INDEX ix_jobs_status_created_at ON jobs (status, created_at DESC, id DESC);

-- ---------------------------------------------------------------------------
-- Caches (ST-3): content-addressed, shared across collections, keyed by hash
-- only, so an entry never reveals which document produced it. No FK to
-- documents or collections. `last_used_at` is bumped on hit; the nightly job
-- prunes entries unused for N days (default 90).
-- ---------------------------------------------------------------------------

-- embedding_cache: (embedding space, sha256 of the embedding input, I-4)
-- -> vector. Also serves query embeddings (R-8).
--
-- `space_id` refers to embedding_spaces (M-3). That table has its own
-- migration; the FK (ON DELETE CASCADE, so retired-space cleanup drops the
-- space's entries) is added there.
CREATE TABLE embedding_cache (
    space_id     uuid        NOT NULL,
    text_sha256  varchar(64) NOT NULL,
    embedding    vector      NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT pk_embedding_cache PRIMARY KEY (space_id, text_sha256),
    CONSTRAINT ck_embedding_cache_text_sha256_hex CHECK (text_sha256 ~ '^[0-9a-f]{64}$')
);

CREATE INDEX ix_embedding_cache_last_used_at ON embedding_cache (last_used_at);

-- vision_cache: (sha256 of the image bytes, model, sha256 of the prompt)
-- -> figure description (I-5). An empty description is cached too (the
-- block is dropped), so the model isn't asked again.
CREATE TABLE vision_cache (
    image_sha256  varchar(64) NOT NULL,
    -- Model identifier as resolved by the model role (provider-qualified).
    model         text        NOT NULL,
    prompt_sha256 varchar(64) NOT NULL,
    description   text        NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_used_at  timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT pk_vision_cache PRIMARY KEY (image_sha256, model, prompt_sha256),
    CONSTRAINT ck_vision_cache_image_sha256_hex CHECK (image_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_vision_cache_prompt_sha256_hex CHECK (prompt_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_vision_cache_model_not_blank CHECK (btrim(model) <> '')
);

CREATE INDEX ix_vision_cache_last_used_at ON vision_cache (last_used_at);

-- context_cache: (key, model, sha256 of the prompt) -> context sentence
-- (CR-4). key = sha256(title + heading path + chunk text + window text), or
-- + document hash for `document` scope. Failures are not cached (CR-6).
CREATE TABLE context_cache (
    key           varchar(64) NOT NULL,
    model         text        NOT NULL,
    prompt_sha256 varchar(64) NOT NULL,
    sentence      text        NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_used_at  timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT pk_context_cache PRIMARY KEY (key, model, prompt_sha256),
    CONSTRAINT ck_context_cache_key_hex CHECK (key ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_context_cache_prompt_sha256_hex CHECK (prompt_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_context_cache_model_not_blank CHECK (btrim(model) <> ''),
    CONSTRAINT ck_context_cache_sentence_not_empty CHECK (btrim(sentence) <> '')
);

CREATE INDEX ix_context_cache_last_used_at ON context_cache (last_used_at);

-- ---------------------------------------------------------------------------
-- figures (M-4, FAB-141): stored only when the collection has
-- `pipeline.keep_figures` on. Downscaled to <= 1024 px and re-encoded as
-- PNG/WebP; written in the chunk-replacement transaction (I-6) and removed
-- with the document or collection (ST-2). Served by GET /search/figures/{id}.
-- ---------------------------------------------------------------------------
CREATE TABLE figures (
    id            uuid        NOT NULL DEFAULT gen_random_uuid(),
    document_id   uuid        NOT NULL,
    -- Denormalized for the ACL check on GET /search/figures/{id}.
    collection_id uuid        NOT NULL,
    -- 0-based figure number within the document (chunk metadata.figures).
    n             integer     NOT NULL,
    mime          varchar(20) NOT NULL,
    width         integer     NOT NULL,
    height        integer     NOT NULL,
    data          bytea       NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT pk_figures PRIMARY KEY (id),
    CONSTRAINT fk_figures_document_id_documents
        FOREIGN KEY (document_id) REFERENCES documents (id) ON DELETE CASCADE,
    CONSTRAINT fk_figures_collection_id_collections
        FOREIGN KEY (collection_id) REFERENCES collections (id) ON DELETE CASCADE,
    -- Also serves lookups and deletes by document_id (leading column).
    CONSTRAINT uq_figures_document_id UNIQUE (document_id, n),
    CONSTRAINT ck_figures_n_non_negative CHECK (n >= 0),
    CONSTRAINT ck_figures_mime_valid CHECK (mime IN ('image/png', 'image/webp')),
    CONSTRAINT ck_figures_width_valid CHECK (width BETWEEN 1 AND 1024),
    CONSTRAINT ck_figures_height_valid CHECK (height BETWEEN 1 AND 1024),
    CONSTRAINT ck_figures_data_not_empty CHECK (octet_length(data) > 0)
);

CREATE INDEX ix_figures_collection_id ON figures (collection_id);
