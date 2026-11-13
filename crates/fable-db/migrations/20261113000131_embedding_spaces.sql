-- FAB-31: embedding spaces and per-space vectors (spec §7 M-3, §5.2 S-13).
--
-- An embedding space is one (connection, model, dim). Vectors live in
-- `chunk_embeddings` in an untyped `vector` column, so spaces of different
-- dimensions share one table. Each space gets its own partial expression HNSW
-- index, created and dropped at runtime by fable_db::create_space_index /
-- drop_space_index (not here: spaces are rows, not schema):
--
--   CREATE INDEX ix_chunk_embeddings_hnsw_space_<id> ON chunk_embeddings
--     USING hnsw ((embedding::vector(<dim>)) vector_cosine_ops)
--     WHERE space_id = <id>;
--
-- Queries must repeat that expression and predicate with literals (see
-- fable_db::space_vector_expr / space_predicate) for the planner to use it.
-- The cast in the index expression also rejects vectors of the wrong
-- dimension for that space at insert time.

-- pgvector. IF NOT EXISTS: the Python schema converted in place (FAB-134)
-- already has it, and pgvector/pgvector images may preinstall it.
CREATE EXTENSION IF NOT EXISTS vector;

-- ---------------------------------------------------------------------------
-- embedding_spaces
-- ---------------------------------------------------------------------------
CREATE TABLE embedding_spaces (
    -- Small integer id: it is inlined as a literal into each space's index
    -- predicate and index name.
    id            bigint      GENERATED ALWAYS AS IDENTITY,
    -- Model-provider connection that produces the vectors. NULL only for
    -- providers without a connection (the built-in `fake` embedder) and for
    -- the space converted from the Python schema until a connection exists.
    connection_id uuid,
    model         text        NOT NULL,
    -- Fixed for the life of the space; the HNSW index is built for it.
    -- pgvector HNSW indexes `vector` up to 2000 dimensions.
    dim           integer     NOT NULL,
    -- building | active | retired (M-6: TEXT + Rust enum).
    status        text        NOT NULL DEFAULT 'building',
    created_at    timestamptz NOT NULL DEFAULT now(),
    activated_at  timestamptz,
    -- When the space stopped being active; the grace period before it is
    -- dropped runs from here (M-3).
    retired_at    timestamptz,

    CONSTRAINT pk_embedding_spaces PRIMARY KEY (id),
    -- Deleting a connection a space uses is refused (S-8 -> 409).
    CONSTRAINT fk_embedding_spaces_connection_id_connections
        FOREIGN KEY (connection_id) REFERENCES connections (id) ON DELETE RESTRICT,
    CONSTRAINT ck_embedding_spaces_model_not_blank CHECK (btrim(model) <> ''),
    CONSTRAINT ck_embedding_spaces_dim_range CHECK (dim BETWEEN 1 AND 2000),
    CONSTRAINT ck_embedding_spaces_status_valid
        CHECK (status IN ('building', 'active', 'retired')),
    CONSTRAINT ck_embedding_spaces_active_has_activated_at
        CHECK (status <> 'active' OR activated_at IS NOT NULL),
    CONSTRAINT ck_embedding_spaces_retired_has_retired_at
        CHECK (status <> 'retired' OR retired_at IS NOT NULL)
);

-- At most one active space at any moment. A plain unique index is checked
-- per row, so an activation flip retires the old space first, then activates
-- the new one, in one transaction.
CREATE UNIQUE INDEX uq_embedding_spaces_one_active
    ON embedding_spaces ((true)) WHERE status = 'active';

CREATE INDEX ix_embedding_spaces_connection_id
    ON embedding_spaces (connection_id) WHERE connection_id IS NOT NULL;

-- At least one active space once any space has ever been activated (or the
-- active one was deleted): checked at commit, so a flip may pass through zero active spaces mid-transaction.
-- Before the first activation (fresh install, first space still building)
-- there is legitimately none (S-15).
CREATE FUNCTION embedding_spaces_check_active() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF (EXISTS (SELECT 1 FROM embedding_spaces WHERE activated_at IS NOT NULL)
        OR (TG_OP = 'DELETE' AND OLD.status = 'active'))
       AND NOT EXISTS (SELECT 1 FROM embedding_spaces WHERE status = 'active') THEN
        RAISE EXCEPTION 'exactly one embedding space must be active'
            USING ERRCODE = 'check_violation',
                  CONSTRAINT = 'ck_embedding_spaces_exactly_one_active';
    END IF;
    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER tr_embedding_spaces_exactly_one_active
    AFTER INSERT OR UPDATE OR DELETE ON embedding_spaces
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION embedding_spaces_check_active();

-- What produces the vectors (and therefore the index) never changes; a new
-- model or dimension is a new space (S-13 blue/green).
CREATE FUNCTION embedding_spaces_immutable_identity() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.dim <> OLD.dim OR NEW.model <> OLD.model THEN
        RAISE EXCEPTION 'embedding space % model/dim cannot change; create a new space', OLD.id
            USING ERRCODE = 'check_violation',
                  CONSTRAINT = 'ck_embedding_spaces_identity_immutable';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER tr_embedding_spaces_immutable_identity
    BEFORE UPDATE OF dim, model ON embedding_spaces
    FOR EACH ROW EXECUTE FUNCTION embedding_spaces_immutable_identity();

-- ---------------------------------------------------------------------------
-- chunk_embeddings: one vector per (chunk, space)
-- ---------------------------------------------------------------------------
CREATE TABLE chunk_embeddings (
    chunk_id  uuid   NOT NULL,
    space_id  bigint NOT NULL,
    -- Untyped: the dimension is the space's `dim`, enforced by the cast in
    -- the space's index expression.
    embedding vector NOT NULL,

    CONSTRAINT pk_chunk_embeddings PRIMARY KEY (chunk_id, space_id),
    -- Chunk replacement (I-6) and document/collection deletes cascade here.
    CONSTRAINT fk_chunk_embeddings_chunk_id_chunks
        FOREIGN KEY (chunk_id) REFERENCES chunks (id) ON DELETE CASCADE,
    -- Dropping a retired space removes its vectors.
    CONSTRAINT fk_chunk_embeddings_space_id_embedding_spaces
        FOREIGN KEY (space_id) REFERENCES embedding_spaces (id) ON DELETE CASCADE
);

-- Re-embed progress (count per space), space deletes, "chunks missing a
-- vector in space X" anti-joins.
CREATE INDEX ix_chunk_embeddings_space_id ON chunk_embeddings (space_id);
