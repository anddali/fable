# Migrations

Embedded into the `fable` binary with `sqlx::migrate!`. Name files
`YYYYMMDDHHMMSS_description.sql`. Never edit the legacy Alembic `migrations/`
directory at the repo root.

`fable_db::migrate` applies them on every boot and in `fable migrate`, holding
a session-level Postgres advisory lock (`classid = 0x0FAB1E00`, second key =
hash of database + schema) on a dedicated connection that is closed afterwards,
so N replicas can start at once and a failed run never leaks the lock.
A database that has migrations this binary does not know (newer build ran
first) is rejected at startup.

Per-embedding-space HNSW indexes are not migrations: they are created and
dropped at runtime by `fable_db::create_space_index[_concurrently]` /
`drop_space_index[_concurrently]` when a space is created or dropped (§7 M-3).
