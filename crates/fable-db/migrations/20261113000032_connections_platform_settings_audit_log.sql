-- FAB-32: connections, platform settings and audit log (spec §5.2 S-1, S-7,
-- S-8, S-10, S-12, S-16; §7 M-4, M-6).
--
-- Status/kind columns are TEXT backed by Rust enums (M-6); the CHECKs below
-- only guard the closed sets the spec defines. Kinds are open (plugins).

-- ---------------------------------------------------------------------------
-- connections: named, reusable credentials for one external system (S-1).
-- ---------------------------------------------------------------------------
CREATE TABLE connections (
    id                    uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Connector / model-provider kind, e.g. `confluence`, `azure-openai`.
    kind                  text        NOT NULL
                                      CONSTRAINT connections_kind_format
                                      CHECK (kind ~ '^[a-z][a-z0-9_-]*$'),
    name                  text        NOT NULL
                                      CONSTRAINT connections_name_not_blank
                                      CHECK (btrim(name) <> ''),
    -- Non-secret settings only (base URL, tenant id, client id, cloud id, ...).
    config                jsonb       NOT NULL DEFAULT '{}'::jsonb
                                      CONSTRAINT connections_config_object
                                      CHECK (jsonb_typeof(config) = 'object'),
    -- AES-256-GCM (S-4): ciphertext includes the 16-byte tag; 12-byte nonce;
    -- version of the master key that sealed it. All three or none.
    secret_ciphertext     bytea,
    secret_nonce          bytea,
    key_version           integer,
    -- Who may attach sources / use the connection (S-7): `admins`,
    -- `all_users`, or `groups` with the Entra group ids in usable_by_groups.
    usable_by             text        NOT NULL DEFAULT 'admins'
                                      CONSTRAINT connections_usable_by_valid
                                      CHECK (usable_by IN ('admins', 'all_users', 'groups')),
    usable_by_groups      text[]      NOT NULL DEFAULT '{}',
    -- enabled | disabled | needs_attention (S-8, S-16, CF-10).
    status                text        NOT NULL DEFAULT 'enabled'
                                      CONSTRAINT connections_status_valid
                                      CHECK (status IN ('enabled', 'disabled', 'needs_attention')),
    -- Human-readable reason shown with `needs_attention` / `disabled`,
    -- e.g. "service-account credentials required". Never contains secrets.
    status_reason         text,
    -- Expiry of the stored credential where the provider has one (S-16).
    credential_expires_at timestamptz,
    created_by            text,
    updated_by            text,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    -- Last "test connection" result (S-5).
    last_test_at          timestamptz,
    last_test_ok          boolean,
    last_test_error       text,

    CONSTRAINT connections_secret_complete CHECK (
        (secret_ciphertext IS NULL AND secret_nonce IS NULL AND key_version IS NULL)
        OR (secret_ciphertext IS NOT NULL AND secret_nonce IS NOT NULL AND key_version IS NOT NULL)
    ),
    CONSTRAINT connections_secret_nonce_len
        CHECK (secret_nonce IS NULL OR octet_length(secret_nonce) = 12),
    CONSTRAINT connections_secret_ciphertext_len
        CHECK (secret_ciphertext IS NULL OR octet_length(secret_ciphertext) >= 16),
    CONSTRAINT connections_key_version_positive
        CHECK (key_version IS NULL OR key_version > 0),
    CONSTRAINT connections_usable_by_groups_consistent CHECK (
        (usable_by = 'groups') = (cardinality(usable_by_groups) > 0)
    ),
    CONSTRAINT connections_last_test_consistent CHECK (
        (last_test_at IS NULL AND last_test_ok IS NULL AND last_test_error IS NULL)
        OR (last_test_at IS NOT NULL AND last_test_ok IS NOT NULL)
    )
);

COMMENT ON TABLE connections IS
    'Portal-managed credentials (spec S-1). Secrets are AES-256-GCM sealed; never returned by the API.';

-- Names are unique ignoring case and surrounding whitespace, so "OpenAI" and
-- "openai " cannot both exist. The API maps this violation to 409.
CREATE UNIQUE INDEX connections_name_key ON connections (lower(btrim(name)));
CREATE INDEX connections_kind_idx ON connections (kind);
-- Groups lookup for "connections this user may use" (S-7).
CREATE INDEX connections_usable_by_groups_idx ON connections USING gin (usable_by_groups);
-- Expiry sweep / 30-day warning (S-16).
CREATE INDEX connections_credential_expires_at_idx
    ON connections (credential_expires_at)
    WHERE credential_expires_at IS NOT NULL;
-- `fable rotate-master-key` finds rows still sealed with an old key.
CREATE INDEX connections_key_version_idx
    ON connections (key_version)
    WHERE key_version IS NOT NULL;

-- ---------------------------------------------------------------------------
-- platform_settings: one row per setting group (S-12). A missing row means
-- "not configured" (e.g. no embedding model yet, S-15) or "use built-in
-- defaults" (limits, indexing defaults).
-- ---------------------------------------------------------------------------
CREATE TABLE platform_settings (
    key           text        PRIMARY KEY
                              CONSTRAINT platform_settings_key_valid
                              CHECK (key IN (
                                  'embedding',
                                  'chat',
                                  'vision_default',
                                  'context_default',
                                  'indexing_defaults',
                                  'limits'
                              )),
    value         jsonb       NOT NULL
                              CONSTRAINT platform_settings_value_object
                              CHECK (jsonb_typeof(value) = 'object'),
    -- The connection a model setting points at, kept in step with
    -- value->>'connection_id'. Deleting a connection a setting uses is
    -- refused (S-8) and reported to the admin as a dependent.
    connection_id uuid        GENERATED ALWAYS AS ((value ->> 'connection_id')::uuid) STORED
                              CONSTRAINT platform_settings_connection_id_fkey
                              REFERENCES connections (id) ON DELETE RESTRICT,
    updated_by    text,
    updated_at    timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE platform_settings IS
    'Singleton setting groups edited in the portal (spec S-12): embedding, chat, vision/context defaults, indexing defaults, limits.';

CREATE INDEX platform_settings_connection_id_idx
    ON platform_settings (connection_id)
    WHERE connection_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- audit_log: append-only record of admin changes (S-10, PS-5). entity_id is
-- not a foreign key on purpose: the trail outlives the entity.
-- ---------------------------------------------------------------------------
CREATE TABLE audit_log (
    id         bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at         timestamptz NOT NULL DEFAULT now(),
    -- NULL for system actions (env import C-4, master-key rotation).
    actor_oid  text,
    actor_name text,
    -- e.g. `create`, `update`, `delete`, `test`, `rotate_key`.
    action     text        NOT NULL
                           CONSTRAINT audit_log_action_not_blank CHECK (btrim(action) <> ''),
    -- e.g. `connection`, `platform_setting`, `collection_acl`, `api_key`,
    -- `pipeline`.
    entity     text        NOT NULL
                           CONSTRAINT audit_log_entity_not_blank CHECK (btrim(entity) <> ''),
    entity_id  text,
    -- Field-level diff; secret values never appear, only "secret changed".
    diff       jsonb       NOT NULL DEFAULT '{}'::jsonb
                           CONSTRAINT audit_log_diff_object CHECK (jsonb_typeof(diff) = 'object')
);

COMMENT ON TABLE audit_log IS
    'Append-only audit trail (spec S-10). Rows are never updated; diffs never contain secret values.';

-- GET /admin/audit-log: newest first, keyset paging on (at, id).
CREATE INDEX audit_log_at_idx ON audit_log (at DESC, id DESC);
CREATE INDEX audit_log_entity_idx ON audit_log (entity, entity_id, at DESC);
CREATE INDEX audit_log_actor_idx ON audit_log (actor_oid, at DESC) WHERE actor_oid IS NOT NULL;

-- Audit rows are immutable. DELETE stays possible for a retention job.
CREATE FUNCTION audit_log_reject_update() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_log rows are append-only'
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER audit_log_no_update
    BEFORE UPDATE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_reject_update();
