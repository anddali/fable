-- FAB-41: master key state (spec §4 C-7, §5.2 S-4).
--
-- One row describing the master key that currently seals every secret:
-- its key_version and a key-check value (a fixed plaintext sealed with
-- AES-256-GCM under that key), so startup can tell a missing or wrong
-- RAG_MASTER_KEY apart from a correct one before any secret is touched.
--
-- In `dev` without RAG_MASTER_KEY an auto-generated key is kept in dev_key
-- (C-7). It is cleared as soon as RAG_MASTER_KEY takes over (startup with
-- the same key, or `fable rotate-master-key`). Outside `dev` it is NULL.
CREATE TABLE master_key_state (
    -- Singleton: at most one row.
    singleton      boolean     PRIMARY KEY DEFAULT true
                               CONSTRAINT master_key_state_singleton CHECK (singleton),
    key_version    integer     NOT NULL
                               CONSTRAINT master_key_state_key_version_positive
                               CHECK (key_version > 0),
    -- Key-check value: ciphertext (with 16-byte GCM tag) and 12-byte nonce.
    check_ciphertext bytea     NOT NULL
                               CONSTRAINT master_key_state_check_len
                               CHECK (octet_length(check_ciphertext) >= 16),
    check_nonce    bytea       NOT NULL
                               CONSTRAINT master_key_state_check_nonce_len
                               CHECK (octet_length(check_nonce) = 12),
    -- Raw 32-byte auto-generated development key, dev only (C-7).
    dev_key        bytea
                               CONSTRAINT master_key_state_dev_key_len
                               CHECK (dev_key IS NULL OR octet_length(dev_key) = 32),
    created_at     timestamptz NOT NULL DEFAULT now(),
    rotated_at     timestamptz
);

COMMENT ON TABLE master_key_state IS
    'Current master key version and key-check value (spec C-7, S-4). Never holds RAG_MASTER_KEY itself; dev_key only in dev.';
