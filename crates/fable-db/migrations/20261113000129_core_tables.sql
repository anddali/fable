-- FAB-29: core tables (spec §7 M-1, M-2, M-5, M-6; §10.1, §10.3, R-7, F-6).
--
-- Fresh schema for new installs. Table, column and constraint names match the
-- Python (Alembic) schema wherever the semantics are unchanged, so the
-- in-place conversion (FAB-134) only has to touch what M-2 changes. Constraint
-- names follow the Python naming convention (pk_/uq_/fk_/ck_/ix_).
--
-- Status/role columns are TEXT-like strings backed by Rust enums (M-6), never
-- Postgres enums. `updated_at` is maintained by the application, as today.
--
-- chunks, embedding spaces and figures are created by their own migrations;
-- they hang off `documents` / `collections` with ON DELETE CASCADE (ST-2).

-- ---------------------------------------------------------------------------
-- collections (M-1; `pipeline` gains the I-8 settings, validated in Rust).
-- ---------------------------------------------------------------------------
CREATE TABLE collections (
    id           uuid         NOT NULL DEFAULT gen_random_uuid(),
    name         varchar(200) NOT NULL,
    description  text         NOT NULL DEFAULT '',
    -- "private": ACL only. "public": every authenticated user gets viewer.
    visibility   varchar(10)  NOT NULL DEFAULT 'private',
    -- Pipeline config (I-8): parser, transformers, chunker, contextual,
    -- vision_llm, keep_figures, fts_language. Validated by fable-core types.
    pipeline     jsonb        NOT NULL DEFAULT '{}'::jsonb,
    -- Standard 5-field cron expression evaluated by the beat.
    sync_cron    varchar(100) NOT NULL DEFAULT '0 2 * * *',
    sync_enabled boolean      NOT NULL DEFAULT true,
    next_sync_at timestamptz,
    created_by   varchar(100) NOT NULL DEFAULT '',
    created_at   timestamptz  NOT NULL DEFAULT now(),
    updated_at   timestamptz  NOT NULL DEFAULT now(),

    CONSTRAINT pk_collections PRIMARY KEY (id),
    CONSTRAINT uq_collections_name UNIQUE (name),
    CONSTRAINT ck_collections_visibility_valid CHECK (visibility IN ('private', 'public')),
    CONSTRAINT ck_collections_pipeline_object CHECK (jsonb_typeof(pipeline) = 'object')
);

-- Beat: due collections.
CREATE INDEX ix_collections_next_sync_at
    ON collections (next_sync_at)
    WHERE sync_enabled;

-- ---------------------------------------------------------------------------
-- collection_principals: collection ACL (M-1).
-- ---------------------------------------------------------------------------
CREATE TABLE collection_principals (
    id             uuid         NOT NULL DEFAULT gen_random_uuid(),
    collection_id  uuid         NOT NULL,
    -- user | group
    principal_type varchar(10)  NOT NULL,
    -- Entra object id.
    principal_id   varchar(100) NOT NULL,
    -- owner | editor | viewer
    role           varchar(10)  NOT NULL,
    created_at     timestamptz  NOT NULL DEFAULT now(),
    updated_at     timestamptz  NOT NULL DEFAULT now(),

    CONSTRAINT pk_collection_principals PRIMARY KEY (id),
    CONSTRAINT fk_collection_principals_collection_id_collections
        FOREIGN KEY (collection_id) REFERENCES collections (id) ON DELETE CASCADE,
    CONSTRAINT uq_collection_principals_collection_id
        UNIQUE (collection_id, principal_type, principal_id),
    CONSTRAINT ck_collection_principals_principal_type_valid
        CHECK (principal_type IN ('user', 'group')),
    CONSTRAINT ck_collection_principals_role_valid
        CHECK (role IN ('owner', 'editor', 'viewer'))
);

CREATE INDEX ix_collection_principals_collection_id ON collection_principals (collection_id);
-- "Which collections can this user / these groups read?"
CREATE INDEX ix_collection_principals_principal
    ON collection_principals (principal_type, principal_id);

-- ---------------------------------------------------------------------------
-- sources (M-2, S-11): `config` holds `connection_id` + target settings. The
-- referenced connection is exposed as a generated column with an FK, so a
-- connection in use cannot be deleted (S-8). Upload sources have no
-- connection (NULL).
-- ---------------------------------------------------------------------------
CREATE TABLE sources (
    id             uuid         NOT NULL DEFAULT gen_random_uuid(),
    collection_id  uuid         NOT NULL,
    name           varchar(200) NOT NULL,
    -- Registry name of the connector plugin ("confluence", "upload", ...).
    connector      varchar(100) NOT NULL,
    -- Validated against the connector's typed config. Never holds secrets.
    config         jsonb        NOT NULL DEFAULT '{}'::jsonb,
    connection_id  uuid         GENERATED ALWAYS AS ((config ->> 'connection_id')::uuid) STORED,
    enabled        boolean      NOT NULL DEFAULT true,
    -- Connector-owned incremental cursor state.
    state          jsonb        NOT NULL DEFAULT '{}'::jsonb,
    last_synced_at timestamptz,
    created_at     timestamptz  NOT NULL DEFAULT now(),
    updated_at     timestamptz  NOT NULL DEFAULT now(),

    CONSTRAINT pk_sources PRIMARY KEY (id),
    CONSTRAINT fk_sources_collection_id_collections
        FOREIGN KEY (collection_id) REFERENCES collections (id) ON DELETE CASCADE,
    CONSTRAINT fk_sources_connection_id_connections
        FOREIGN KEY (connection_id) REFERENCES connections (id) ON DELETE RESTRICT,
    CONSTRAINT uq_sources_collection_id UNIQUE (collection_id, name),
    CONSTRAINT ck_sources_config_object CHECK (jsonb_typeof(config) = 'object'),
    CONSTRAINT ck_sources_state_object CHECK (jsonb_typeof(state) = 'object')
);

CREATE INDEX ix_sources_collection_id ON sources (collection_id);
-- Dependents of a connection (S-8 delete check, "used by" lists).
CREATE INDEX ix_sources_connection_id ON sources (connection_id) WHERE connection_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- documents (M-2): no blob_key; `source_version` is the connector's change
-- marker, `content_hash` the hash of the parsed text (I-3).
-- ---------------------------------------------------------------------------
CREATE TABLE documents (
    id                uuid          NOT NULL DEFAULT gen_random_uuid(),
    source_id         uuid          NOT NULL,
    -- Denormalized for retrieval-time ACL filtering without a join.
    collection_id     uuid          NOT NULL,
    external_id       varchar(800)  NOT NULL,
    title             varchar(1000) NOT NULL DEFAULT '',
    uri               varchar(2000) NOT NULL DEFAULT '',
    mime_type         varchar(200)  NOT NULL DEFAULT 'text/plain',
    -- Connector change marker (page version, blob SHA, eTag, upload sha256).
    source_version    text          NOT NULL DEFAULT '',
    -- sha256 of the parsed text, for skip-if-identical (I-3).
    content_hash      varchar(64)   NOT NULL DEFAULT '',
    size_bytes        bigint        NOT NULL DEFAULT 0,
    doc_metadata      jsonb         NOT NULL DEFAULT '{}'::jsonb,
    -- pending | indexed | failed | deleted
    status            varchar(10)   NOT NULL DEFAULT 'pending',
    -- Last index error, truncated to 4000 chars (I-7).
    error             text          NOT NULL DEFAULT '',
    last_error_at     timestamptz,
    source_updated_at timestamptz,
    indexed_at        timestamptz,
    created_at        timestamptz   NOT NULL DEFAULT now(),
    updated_at        timestamptz   NOT NULL DEFAULT now(),

    CONSTRAINT pk_documents PRIMARY KEY (id),
    CONSTRAINT fk_documents_source_id_sources
        FOREIGN KEY (source_id) REFERENCES sources (id) ON DELETE CASCADE,
    CONSTRAINT fk_documents_collection_id_collections
        FOREIGN KEY (collection_id) REFERENCES collections (id) ON DELETE CASCADE,
    CONSTRAINT uq_documents_source_id UNIQUE (source_id, external_id),
    CONSTRAINT ck_documents_status_valid
        CHECK (status IN ('pending', 'indexed', 'failed', 'deleted')),
    CONSTRAINT ck_documents_doc_metadata_object CHECK (jsonb_typeof(doc_metadata) = 'object')
);

-- Filter indexes (M-5). uq_documents_source_id also serves source_id lookups,
-- but M-5 names the single-column index explicitly and Python had it too.
CREATE INDEX ix_documents_source_id ON documents (source_id);
CREATE INDEX ix_documents_collection_id ON documents (collection_id);
CREATE INDEX ix_documents_source_updated_at ON documents (source_updated_at);
CREATE INDEX ix_documents_mime_type ON documents (mime_type);
-- Status chips / "retry failed" per collection.
CREATE INDEX ix_documents_collection_id_status ON documents (collection_id, status);

-- ---------------------------------------------------------------------------
-- upload_items (M-2, §10.3): no blob_key. The bytes live only in the scratch
-- dir of `scratch_node` until indexed (U-1, U-3).
-- ---------------------------------------------------------------------------
CREATE TABLE upload_items (
    id           uuid         NOT NULL DEFAULT gen_random_uuid(),
    source_id    uuid         NOT NULL,
    filename     varchar(500) NOT NULL,
    mime_type    varchar(200) NOT NULL,
    size_bytes   bigint       NOT NULL,
    -- sha256 of the uploaded file (also the document's source_version).
    content_hash varchar(64)  NOT NULL,
    uploaded_by  varchar(100) NOT NULL DEFAULT '',
    deleted      boolean      NOT NULL DEFAULT false,
    -- staged | indexed | failed | expired
    status       varchar(10)  NOT NULL DEFAULT 'staged',
    -- Node whose scratch dir holds the staged file; its index job is pinned
    -- there. Kept after indexing for diagnostics.
    scratch_node text,
    created_at   timestamptz  NOT NULL DEFAULT now(),
    updated_at   timestamptz  NOT NULL DEFAULT now(),

    CONSTRAINT pk_upload_items PRIMARY KEY (id),
    CONSTRAINT fk_upload_items_source_id_sources
        FOREIGN KEY (source_id) REFERENCES sources (id) ON DELETE CASCADE,
    CONSTRAINT ck_upload_items_status_valid
        CHECK (status IN ('staged', 'indexed', 'failed', 'expired')),
    -- A staged file exists on exactly one node.
    CONSTRAINT ck_upload_items_staged_has_node
        CHECK (status <> 'staged' OR scratch_node IS NOT NULL),
    CONSTRAINT ck_upload_items_size_bytes_non_negative CHECK (size_bytes >= 0)
);

CREATE INDEX ix_upload_items_source_id ON upload_items (source_id);
-- Startup sweep: a restarted node expires its own staged items (U-3).
CREATE INDEX ix_upload_items_scratch_node_staged
    ON upload_items (scratch_node)
    WHERE status = 'staged';

-- ---------------------------------------------------------------------------
-- sync_runs (M-1).
-- ---------------------------------------------------------------------------
CREATE TABLE sync_runs (
    id            uuid        NOT NULL DEFAULT gen_random_uuid(),
    collection_id uuid        NOT NULL,
    -- cron | manual (open set, as today)
    trigger       varchar(10) NOT NULL DEFAULT 'cron',
    -- running | succeeded | failed
    status        varchar(10) NOT NULL DEFAULT 'running',
    started_at    timestamptz NOT NULL,
    finished_at   timestamptz,
    -- {"<source name>": {"seen", "added", "updated", "deleted", "errors": [...]}}
    stats         jsonb       NOT NULL DEFAULT '{}'::jsonb,
    error         text        NOT NULL DEFAULT '',

    CONSTRAINT pk_sync_runs PRIMARY KEY (id),
    CONSTRAINT fk_sync_runs_collection_id_collections
        FOREIGN KEY (collection_id) REFERENCES collections (id) ON DELETE CASCADE,
    CONSTRAINT ck_sync_runs_status_valid CHECK (status IN ('running', 'succeeded', 'failed'))
);

CREATE INDEX ix_sync_runs_collection_id ON sync_runs (collection_id);
-- Sync history, newest first.
CREATE INDEX ix_sync_runs_collection_id_started_at
    ON sync_runs (collection_id, started_at DESC, id DESC);

-- ---------------------------------------------------------------------------
-- api_keys (M-1). Only the SHA-256 of the secret is stored.
-- ---------------------------------------------------------------------------
CREATE TABLE api_keys (
    id           uuid         NOT NULL DEFAULT gen_random_uuid(),
    name         varchar(200) NOT NULL,
    -- First characters of the secret, for display only.
    prefix       varchar(16)  NOT NULL,
    key_hash     varchar(64)  NOT NULL,
    owner_oid    varchar(100) NOT NULL,
    owner_name   varchar(200) NOT NULL DEFAULT '',
    -- JSON array of collection ids.
    scopes       jsonb        NOT NULL DEFAULT '[]'::jsonb,
    revoked      boolean      NOT NULL DEFAULT false,
    expires_at   timestamptz,
    last_used_at timestamptz,
    created_at   timestamptz  NOT NULL DEFAULT now(),
    updated_at   timestamptz  NOT NULL DEFAULT now(),

    CONSTRAINT pk_api_keys PRIMARY KEY (id),
    CONSTRAINT uq_api_keys_key_hash UNIQUE (key_hash),
    CONSTRAINT ck_api_keys_scopes_array CHECK (jsonb_typeof(scopes) = 'array')
);

CREATE INDEX ix_api_keys_owner_oid ON api_keys (owner_oid);

-- ---------------------------------------------------------------------------
-- query_logs (M-1, R-7): one row per /search and /chat. `filters` is new
-- (F-6) and records the search filters used.
-- ---------------------------------------------------------------------------
CREATE TABLE query_logs (
    id             uuid         NOT NULL DEFAULT gen_random_uuid(),
    principal_oid  varchar(100) NOT NULL,
    principal_name varchar(200) NOT NULL DEFAULT '',
    -- sso | api_key
    auth_method    varchar(10)  NOT NULL DEFAULT 'sso',
    -- Not a foreign key: logs outlive deleted keys.
    api_key_id     uuid,
    query          text         NOT NULL,
    mode           varchar(10)  NOT NULL DEFAULT 'hybrid',
    top_k          integer      NOT NULL DEFAULT 0,
    collection_ids jsonb        NOT NULL DEFAULT '[]'::jsonb,
    filters        jsonb        NOT NULL DEFAULT '{}'::jsonb,
    hit_count      integer      NOT NULL DEFAULT 0,
    latency_ms     integer      NOT NULL DEFAULT 0,
    created_at     timestamptz  NOT NULL DEFAULT now(),

    CONSTRAINT pk_query_logs PRIMARY KEY (id),
    CONSTRAINT ck_query_logs_filters_object CHECK (jsonb_typeof(filters) = 'object')
);

CREATE INDEX ix_query_logs_principal_oid ON query_logs (principal_oid);
CREATE INDEX ix_query_logs_created_at ON query_logs (created_at);
