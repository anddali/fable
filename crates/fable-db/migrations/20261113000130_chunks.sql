-- FAB-30: chunks (spec §7 M-2, M-5; §10.6, §11, I-4, I-6, CR-3).
--
-- Column names match the Python (Alembic) `chunks` table where the semantics
-- are unchanged, so the in-place conversion (FAB-134) only has to move
-- `embedding` into an embedding space, drop `tsv` and fill the new columns.
--
-- Vectors live in `chunk_embeddings` (M-3, own migration), not here.

CREATE TABLE chunks (
    id                uuid        NOT NULL DEFAULT gen_random_uuid(),
    document_id       uuid        NOT NULL,
    -- Denormalized for the ACL hard filter (R-1) without a join.
    collection_id     uuid        NOT NULL,
    -- 0-based position within the document.
    ordinal           integer     NOT NULL,
    -- Source text only: no breadcrumb, no context sentence (M-2, CH-4, CR-3).
    text              text        NOT NULL,
    token_count       integer     NOT NULL DEFAULT 0,
    -- heading_path, page/slide spans and other chunk-level metadata.
    chunk_metadata    jsonb       NOT NULL DEFAULT '{}'::jsonb,
    -- Block map over `text`: [{offset, len, type, level?, page?, slide?}].
    -- Lets re-chunk / re-context run without re-fetching (ST-4, §11).
    blocks            jsonb       NOT NULL DEFAULT '[]'::jsonb,
    -- Contextual-retrieval sentence (§10.6); NULL when disabled or when the
    -- context model failed (CR-6, "chunks missing context").
    context           text,
    -- sha256 (hex) of the full embedding input (I-4); key into embedding_cache.
    -- Empty only for rows converted from the Python schema until re-embedded.
    embed_text_sha256 varchar(64) NOT NULL DEFAULT '',
    -- Maintained by the indexer (not generated): title A, context B, text C,
    -- in the collection's fts_language (M-2, M-5). Build it with chunk_fts().
    fts               tsvector    NOT NULL DEFAULT ''::tsvector,
    created_at        timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT pk_chunks PRIMARY KEY (id),
    CONSTRAINT fk_chunks_document_id_documents
        FOREIGN KEY (document_id) REFERENCES documents (id) ON DELETE CASCADE,
    CONSTRAINT fk_chunks_collection_id_collections
        FOREIGN KEY (collection_id) REFERENCES collections (id) ON DELETE CASCADE,
    -- Also serves lookups by document_id (leading column), so there is no
    -- separate ix_chunks_document_id. Chunk replacement deletes the old rows
    -- before inserting the new ones in the same transaction (I-6).
    CONSTRAINT uq_chunks_document_id UNIQUE (document_id, ordinal),
    CONSTRAINT ck_chunks_ordinal_non_negative CHECK (ordinal >= 0),
    CONSTRAINT ck_chunks_token_count_non_negative CHECK (token_count >= 0),
    CONSTRAINT ck_chunks_chunk_metadata_object CHECK (jsonb_typeof(chunk_metadata) = 'object'),
    CONSTRAINT ck_chunks_blocks_array CHECK (jsonb_typeof(blocks) = 'array'),
    CONSTRAINT ck_chunks_context_not_empty CHECK (context IS NULL OR context <> ''),
    CONSTRAINT ck_chunks_embed_text_sha256_hex
        CHECK (embed_text_sha256 ~ '^([0-9a-f]{64})?$')
);

-- ACL-filtered scans and per-collection counts (R-1, CR-6, PS-3).
CREATE INDEX ix_chunks_collection_id ON chunks (collection_id);
-- Lexical leg of hybrid search (R-5).
CREATE INDEX ix_chunks_fts ON chunks USING gin (fts);

-- The one definition of the weighted fts value (M-2): title A, context B,
-- text C. The indexer computes `fts` with it on insert (e.g. COPY into a
-- staging table, then INSERT ... SELECT chunk_fts(...)), and re-context /
-- fts_language changes recompute it. NULL title/context contribute nothing.
CREATE FUNCTION chunk_fts(config regconfig, title text, context text, body text)
    RETURNS tsvector
    LANGUAGE sql
    IMMUTABLE
    PARALLEL SAFE
    RETURN setweight(to_tsvector(config, coalesce(title, '')), 'A')
        || setweight(to_tsvector(config, coalesce(context, '')), 'B')
        || setweight(to_tsvector(config, coalesce(body, '')), 'C');
