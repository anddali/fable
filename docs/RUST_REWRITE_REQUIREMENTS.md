# Rust Rewrite — Requirements

Status: draft · Date: 2026-10-13

## 1. Goal

Re-implement the RAG platform (today: Python `rag-api` + `rag-worker` + Procrastinate + Alembic, 3 containers + Postgres) as a **single Rust binary shipped in a single Docker image**. That one container serves the REST API, the MCP endpoint, the management portal, and runs the background sync/index workers.

Three design changes come with the rewrite:

1. **Postgres is the only state, and only for what's needed.** Postgres (with pgvector) stays **external**: a separate `pgvector/pgvector:pg17` container locally, a managed instance in prod. The app container is **stateless**, with no volume.
2. **No original documents are stored anywhere**: not in a blob store, not in Postgres. Sources are read at index time, parsed in a streaming fashion, and discarded. Only the search index (chunks + vectors) and small derived data are kept (§6).
3. **Connections, models and their credentials are managed in the portal UI**, stored in the database with secrets encrypted. They are no longer configured through environment variables. Env vars are reduced to a small bootstrap set (§4, §5).

**Docling is dropped.** Layout-aware parsing is replaced by native Rust parsers plus the vision-LLM step for images.

### Non-goals

- The portal is **redesigned** (§20: light theme, scalable lists, admin area), not just extended. Whether to keep the current no-build vanilla-JS SPA or move to a component framework built in the Docker builder stage is open decision §18 #6. Either way, the output is static files embedded in the binary.
- No embedded database (SQLite or similar).
- No runtime-loadable third-party plugins in v1 (see §8).
- No mixed Python/Rust operation on one database (see §17 for the cutover).

## 2. Deployment shape

```
docker compose
 ├── db    pgvector/pgvector:pg17          (unchanged, the only volume)
 └── app   fable (single Rust binary)      :8000   (no volume)
           ├─ HTTP: REST API, /mcp, /ui/, /healthz, /readyz
           ├─ startup: run DB migrations (advisory-locked)
           ├─ background: beat (1 min) · sync workers · index workers
           └─ child processes: parse workers (§10.4)
```

| ID | Requirement |
|---|---|
| D-1 | One image, one process tree. Default command runs **all roles** (API + workers + migrations on boot). |
| D-2 | `--role api\|worker\|all` (env `RAG_ROLE`) so API and workers can be scaled separately later without code changes. |
| D-3 | `fable migrate` subcommand runs migrations and exits (for CI/ops). Startup migrations are guarded by a Postgres advisory lock so N replicas can boot concurrently. |
| D-4 | Runtime image: `debian:bookworm-slim` or distroless `cc`, non-root uid 1000, plus only `git`, `ca-certificates` and the `libpdfium` shared lib. Target image size **< 150 MB**. |
| D-5 | Healthcheck must not need `curl`: `fable healthcheck` subcommand hits `/healthz` locally. |
| D-6 | Graceful shutdown on SIGTERM: stop taking jobs, finish or release in-flight jobs within a configurable timeout, drain HTTP. |
| D-7 | `docker-compose.yml` reduced to `db` + `app`. The `migrate` and `worker` services and the `appdata` volume are removed. |
| D-8 | **Scratch space** (`RAG_SCRATCH_DIR`, default `/tmp/fable`) holds only ephemeral files: in-flight downloads, pending uploads, partial git clones. Losing it at restart must never lose indexed data. It's capped by `RAG_SCRATCH_MAX_BYTES` (default 10 GB); index jobs wait for space instead of failing. |

## 3. Tech stack (decided)

| Concern | Crate / tool |
|---|---|
| HTTP | `axum`, `tower`, `tower-http` (trace, compression, CORS, limits) |
| Async runtime | `tokio` |
| DB | `sqlx` (Postgres, compile-time-checked queries, offline mode for CI) + `pgvector` crate |
| Migrations | `sqlx::migrate!` embedded in the binary |
| HTTP client | `reqwest` (rustls, streaming bodies), one shared client per process |
| JWT / JWKS | `jsonwebtoken` + an in-process JWKS cache (1 h TTL, refetch on unknown `kid`) |
| Secrets | `aes-gcm` (AES-256-GCM), `secrecy` + `zeroize` for in-memory handling |
| Cron | `croner` (5-field, same semantics as `croniter` for the expressions we accept) |
| MCP | `rmcp` (official Rust MCP SDK), streamable-HTTP transport mounted at `/mcp` |
| Config (bootstrap) | `figment` with env provider (`RAG_` prefix, `__` nesting) |
| JSON schema / validation | `serde` + `schemars` (config schemas drive the portal forms) + `garde`/`validator` |
| Logging | `tracing` + `tracing-subscriber` JSON formatter |
| Static UI | `rust-embed` (UI baked into the binary) |
| PDF | `pdfium-render` (text, font/position info, embedded images, page rendering) |
| XLSX / CSV | `calamine`, `csv` |
| DOCX / PPTX | `zip` + `quick-xml` (own minimal OOXML readers) |
| HTML | `scraper` / `html5ever` |
| Markdown | `pulldown-cmark` |
| Git | `git` CLI via `tokio::process` (partial clones, see §8). Moving to `gix` is optional later. |
| Errors | `thiserror` (lib), `anyhow` (bin edges) |

## 4. Bootstrap configuration (env)

Env vars cover only what is needed **before** a database and a signed-in admin exist. Everything else lives in the database and is edited in the portal (§5).

| ID | Requirement |
|---|---|
| C-1 | **Bootstrap env vars:** `RAG_DATABASE_URL`, `RAG_MASTER_KEY`, `RAG_ENVIRONMENT`, `RAG_LOG_LEVEL`, `RAG_ROLE`, `RAG_HTTP__BIND` (default `0.0.0.0:8000`), `RAG_SCRATCH_DIR`, `RAG_SCRATCH_MAX_BYTES`, worker concurrency (`RAG_WORKER__SYNC_CONCURRENCY` = 2, `RAG_WORKER__INDEX_CONCURRENCY` = 4, `RAG_WORKER__PARSE_PROCESSES` = CPU count), `RAG_WORKER__SHUTDOWN_TIMEOUT_SECS` = 30. |
| C-2 | **Sign-in config stays in env**, because you must be able to sign in to reach the portal: `RAG_AUTH__MODE`, `RAG_AUTH__TENANT_ID`, `RAG_AUTH__AUDIENCE`, `RAG_AUTH__SPA_CLIENT_ID`, `RAG_AUTH__API_SCOPE`, `RAG_AUTH__ROLE_MAP`, `RAG_AUTH__DEFAULT_ROLE`. |
| C-3 | **Removed from env** (moved to the portal): `RAG_EMBEDDING__*`, `RAG_CHAT__*`, `RAG_CONFLUENCE__*`, all connector token variables (`CONFLUENCE_API_TOKEN`, `OPENAI_API_KEY`, `SLACK_BOT_TOKEN`, `SHAREPOINT_CLIENT_SECRET`, …), `RAG_BLOB_DIR`, `RAG_CACHE_DIR`. `RAG_MAX_UPLOAD_BYTES` and `RAG_SEARCH_MAX_TOP_K` become platform settings in the portal. |
| C-4 | **One-time import:** on first boot, if the removed variables are still set and no connections exist yet, create the matching connections and model settings from them (secrets encrypted). Log what was imported, and log a warning on later boots that the variables are ignored. |
| C-5 | `RAG_DATABASE_URL` accepts the legacy `postgresql+psycopg://` prefix and normalises it to `postgres://`. |
| C-6 | `RAG_AUTH__MODE=disabled` with `RAG_ENVIRONMENT=prod` is a **startup failure**. |
| C-7 | `RAG_MASTER_KEY` (32 random bytes, base64) is required in every environment except `dev`. In `dev` a key is generated and stored in the database with a loud warning. Missing or wrong key while encrypted secrets exist → startup failure naming the problem, never silent data loss. |

## 5. Connections, models & secrets (portal-managed)

Credentials and model choices are **data**, managed by admins in the portal, not deployment config.

### 5.1 Concepts

- **Connection**: a named, reusable credential for one external system, e.g. "Confluence – service account", "SharePoint – HR tenant", "Slack – eng workspace", "GitHub – read-only token", "OpenAI", "Local Ollama", "Anthropic".
- **Source**: a collection's ingestion target (space, drive, channel list, repo). It **references a connection** and holds only target settings.
- **Model settings**: which connection + model is used for **embeddings** (platform-wide), **chat** (platform-wide), and **vision** (per collection, with a platform default).

### 5.2 Requirements

| ID | Requirement |
|---|---|
| S-1 | `connections` table: `id, kind, name (unique), config jsonb (non-secret: base URL, user email, tenant id, client id, …), secret_ciphertext bytea, secret_nonce, key_version, usable_by, created_by, updated_by, created_at, updated_at, last_test_at, last_test_ok, last_test_error`. |
| S-2 | **Connection kinds** (each with a typed config + secret schema that drives the portal form): `confluence` (**Atlassian service account only**, see §5.3), `sharepoint` (tenant id, client id, client secret), `slack` (bot token), `git` (host, optional username, token), plus the **model provider** kinds in §5.4. |
| S-3 | **Secrets are write-only.** No API response ever contains a secret. Responses show `secret_set: true`, the last 4 characters at most, and `updated_at`. Updating a connection without a secret field keeps the existing secret. |
| S-4 | **Encryption at rest:** AES-256-GCM with `RAG_MASTER_KEY`, a random nonce per value, and `key_version` stored for rotation. `fable rotate-master-key --new-key …` re-encrypts all secrets in one transaction. Plaintext exists only in memory (`SecretString`, zeroized on drop) and is never logged, traced or included in error messages. |
| S-5 | **Test connection:** `POST /admin/connections/{id}/test` (and a "test before save" variant) makes a cheap authenticated call: Confluence per-endpoint scope check through the gateway (§5.3 CF-6), Graph token + site lookup, Slack `auth.test`, `git ls-remote`, `/models` or a 1-token embedding for LLMs. The result is stored on the row and shown in the UI. |
| S-6 | **Who can manage:** only global `admin` can create, edit, test or delete connections and model settings. |
| S-7 | **Who can use:** `usable_by` = `admins` \| `all_users` \| a list of Entra group ids. A collection editor can attach a source only through a connection they may use. This stops a regular user from pointing a powerful service account at content they shouldn't see. |
| S-8 | **Referential safety:** deleting a connection that sources or model settings use returns **409** with the list of dependents. Disabling a connection makes dependent sources fail fast with a clear error. |
| S-9 | **Hot reload:** changes take effect without restart. Every replica caches decrypted connections in memory and invalidates them through `LISTEN/NOTIFY` on change. |
| S-10 | **Audit log:** `audit_log(id, at, actor_oid, actor_name, action, entity, entity_id, diff jsonb)` for connection, model-setting and platform-setting changes, plus ACL and API-key changes. Diffs never include secret values (only "secret changed"). Admin-only `GET /admin/audit-log`. |
| S-11 | **Source config** no longer contains `*_env` fields. It holds `connection_id` plus target settings (Confluence targets, SharePoint site/drive/path, Slack channels, Git repo/branch/globs). The Confluence "named connections" concept is replaced by regular connections. |
| S-12 | **Model settings** (`platform_settings` singleton rows, edited in the portal): **embedding** `{connection_id, model, dim}`; **chat** `{enabled, connection_id, model, temperature, max_tokens, top_k, mode, history_turns}`; **vision default** `{connection_id, model, prompt?}`; **contextual-retrieval default** `{connection_id, model}` (§10.6); **indexing defaults** (the default pipeline for new collections); **limits** `{max_upload_bytes, search_max_top_k}`. A collection's `pipeline.vision_llm` becomes `{connection_id, model, prompt?}` or `null`. |
| S-13 | **Changing the embedding model** is guarded. The portal shows the impact (number of chunks, estimated cost) and asks for confirmation, then runs a **blue/green re-embed** (§7 M-3). Search keeps using the old model until the new one is complete and an admin activates it. No document is re-fetched, because vectors are rebuilt from stored chunk text. |
| S-14 | **Portal pages** added to the SPA: *Settings → Connections* (list, create/edit form generated from JSON Schema, test, usage, delete), *Settings → Models* (embedding / chat / vision default, with the re-embed flow and its progress), *Settings → Limits*, *Settings → Indexing defaults* (default pipeline for new collections, §10.2 I-8), *Settings → Audit log*. Per collection: *Indexing settings* (§11.1), *Evaluation* (§12.3), *Feedback*. The search page gains filter controls fed by `/search/facets`. Chat gains thumbs up/down. The source form replaces raw credential fields with a connection picker filtered by kind and `usable_by`. |
| S-15 | `GET /ui-config` and `/me` expose to the UI whether the caller is admin and which features are configured, e.g. chat enabled, embedding configured. Until an embedding model is configured, the portal shows a setup banner and indexing jobs wait instead of failing. |
| S-16 | **Credential expiry**: connections store the credential's expiry date where the provider has one, e.g. Atlassian scoped tokens. The portal warns admins 30 days before expiry, and expired credentials mark the connection `needs_attention`. |

### 5.3 Confluence: Atlassian service account API (not personal tokens)

The Confluence connector authenticates as an **Atlassian service account** (created in Atlassian Administration → Directory → Service accounts). It does **not** use a person's API token or PAT. Service accounts use a **different API surface** from personal tokens: requests go through the Atlassian platform gateway (`api.atlassian.com`) addressed by the site's **cloud ID**, not to `https://<site>.atlassian.net/wiki`.

> Today's Python connector uses Basic auth (email + classic API token) against the site URL. That's the personal-token style, and the rewrite must not carry it over.

| ID | Requirement |
|---|---|
| CF-1 | **Base URL**: every Confluence REST call (v2 `/wiki/api/v2/...` and any v1 `/wiki/rest/api/...` still needed) goes to `https://api.atlassian.com/ex/confluence/{cloudId}/wiki/...`. Calls to `*.atlassian.net` are never made with service-account credentials, except the unauthenticated cloud-ID lookup in CF-4. |
| CF-2 | **Auth method A, recommended: OAuth 2.0 client credentials (2LO).** The connection stores `client_id` + `client_secret` (encrypted). The connector POSTs `grant_type=client_credentials` to `https://auth.atlassian.com/oauth/token`, gets a **60-minute** bearer access token, and caches it per connection in memory, shared across jobs. It refreshes the token ~5 minutes before expiry, and once on a `401` before failing. The client secret doesn't expire; rotation is a portal action. |
| CF-3 | **Auth method B: scoped service-account API token.** The connection stores the token (encrypted) and its expiry date (Atlassian allows 1–365 days; the portal asks for it). It's sent as `Authorization: Bearer <token>` to the gateway. Expiry warnings follow S-16. |
| CF-4 | **Connection config**: `site_url` (e.g. `https://acme.atlassian.net`, used only for display, for building page links (`webui`) and for parsing pasted links), `cloud_id` (auto-discovered when the admin enters the site URL, via the unauthenticated `GET {site_url}/_edge/tenant_info`; editable; it is **not** the organization ID, which causes 404s), `auth_method` (`oauth_client_credentials` \| `scoped_api_token`). No user-email field. |
| CF-5 | **Personal credentials are rejected.** There's no Basic-auth (email + token) option and no site-URL API mode. The portal form explains that a service account is required and links to Atlassian's service-account docs. |
| CF-6 | **Scopes**: the service account's credential needs read-only granular scopes for spaces, pages (incl. children/hierarchy), page bodies and attachments; never write or delete. The exact scope list is pinned in the connector's docs against Atlassian's scope reference during implementation. **Test connection** (S-5) calls each endpoint the connector uses (space lookup, page list, page body, children, attachment download) and reports which call failed with `401`/`403` and the scope it probably lacks. |
| CF-7 | **Gateway link handling**: pagination cursors (`_links.next`) and attachment download links come back as site-relative paths. The connector rebases them onto the gateway base (CF-1) and never follows them to `*.atlassian.net`. Document `uri`s shown to users are built from `site_url` + `webui`. |
| CF-8 | **Permission boundary**: the service account sees exactly the spaces it's been granted, and that's what can be indexed. Combined with `usable_by` (S-7), admins can create one service-account connection per permission boundary (e.g. a general account plus an HR-only account). |
| CF-9 | **Rate limits**: honour `429` + `Retry-After` from the gateway (reschedule, don't burn retries, per Q-5). Concurrency per connection is capped (default 4). |
| CF-10 | **Migration (C-4)**: legacy `RAG_CONFLUENCE__*` env values (site URL + email + classic token) are imported as a Confluence connection in **`needs_attention`** state, with `site_url` and the discovered `cloud_id` filled in but **no credential**. Its sources stay attached, but syncs are blocked with "service-account credentials required" until an admin enters OAuth client credentials or a scoped token and the connection test passes. |
| CF-11 | Confluence **Data Center / Server** is out of scope (different API and auth). The connection kind is Cloud-only. |

### 5.4 Model providers and model roles

Model configuration has two layers, each with its own admin tab: **providers** (how to reach an LLM service, with credentials) and **roles** (which provider + model does which job).

| ID | Requirement |
|---|---|
| MP-1 | **Provider connection kinds**: `openai` (API key, optional org), `azure-openai` (endpoint, API key or Entra client credentials, API version), `anthropic` (API key), `ollama` (base URL, optional bearer for a proxied instance), `litellm` (proxy base URL, virtual key), `openai-compatible` (base URL, optional key; vLLM, LM Studio, TGI, etc.). All are configured in the portal (*Admin → Model providers*); none via env vars. |
| MP-2 | **Model discovery**: each provider lists its models (`/v1/models`, Ollama `/api/tags`, LiteLLM `/model/info`) with capability hints where the provider exposes them (embedding / chat / vision, context window, embedding dimension). Manual model entry is always possible. The list is cached and refreshable. |
| MP-3 | **Model roles** (*Admin → Model roles* tab), each = provider + model + role parameters: **Embedding** (model, dimension, batch size, concurrency), **Chat** (model, temperature, max tokens, top_k, mode, history turns), **Vision** (model, prompt, max image size, min image px, max images per doc), **Context line** (model, prompt, neighbour window, max tokens, concurrency). Only providers/models whose capabilities fit the role are offered; unknown capability → allowed with a warning. |
| MP-4 | **Role test panel**: each role has a "try it" action. Embedding embeds a sample text and shows dimension and latency. Chat answers a sample question. Vision describes an uploaded sample image. Context line generates a line for a chosen chunk and shows the exact prompt (the chunk plus its neighbours), the generated line and the final text that would be embedded. |
| MP-5 | **Per-collection overrides**: a collection may override Vision and Context line (provider, model, prompt, window) in its indexing settings (§11.1); unset = inherits the role default. Embedding is platform-wide (one active embedding space, §7 M-3). Chat is platform-wide. |
| MP-6 | **Usage metrics** per role and per provider: calls, input/output tokens, errors, p50/p95 latency, cache hit rate, over 1/7/30 days; optional price per 1M tokens on a model to show estimated cost. Shown on the admin dashboard and in the role tab. |
| MP-7 | Changing the **Embedding** role triggers the blue/green re-embed flow (S-13). Changing **Vision** or **Context line** defaults offers to rebuild affected collections (re-context needs no fetch; vision changes need a re-parse). |

## 6. Storage policy: no original documents

**Principle:** the platform keeps the *index*, not the *corpus*. Original files and pages are read from the source when they are indexed and then discarded.

| Data | Stored? | Where |
|---|---|---|
| Original bytes (PDF, DOCX, Confluence HTML, Slack text, git files) | **Never** | Streamed to scratch during one index job, deleted at job end (success or failure). |
| Uploaded files | **Never kept** | Scratch until their index job finishes (§10.3). |
| Full extracted text (parsed document) | **No** | Chunks are non-overlapping, so they already contain the full text. |
| Chunks: source text only (no breadcrumb/context prefix), block map, metadata, ordinal | Yes | `chunks` (see §11: the block map lets chunks be re-cut without re-fetching) |
| Contextual-retrieval sentence per chunk | Yes (one sentence) | `chunks.context` (§10.6) |
| Vectors | Yes, per embedding space | `chunk_embeddings` (§7) |
| Document rows: title, URI, MIME type, size, source version, status | Yes | `documents` |
| Figure images | **Optional, default off** (`pipeline.keep_figures`) | `figures` table, downscaled to ≤ 1024 px, re-encoded PNG/WebP. Without it, only the text description is indexed and search hits carry no image link. |
| Embedding cache | Yes (small) | `embedding_cache(text_sha256, space_id) → vector` |
| Vision cache | Yes (small) | `vision_cache(image_sha256, model, prompt_sha256) → description` |

| ID | Requirement |
|---|---|
| ST-1 | No code path persists source bytes to Postgres or to any durable filesystem. A test asserts that after sync + index, scratch is empty and no table contains payload bytes. |
| ST-2 | All derived rows are removed through **FK `ON DELETE CASCADE`** from `documents` (chunks, embeddings, figures) and from `collections`. There is no blob cleanup code and no GC command. |
| ST-3 | Caches are content-addressed and shared across collections. Entries unused for `N` days (default 90) are pruned by a nightly job. A cache entry never reveals which document produced it; it's keyed by hash only. |
| ST-4 | **Rebuild paths**, from cheapest to most expensive: *re-embed* (embedding model change) works from stored chunk text, with no fetch. *Re-context* (contextual-retrieval setting change) and *re-chunk* (chunk size or other chunker setting change) are rebuilt **from the stored chunks' text and block maps**, with no fetch and no re-upload (§11). *Re-parse* (parser or transformer change) needs a **force resync** that fetches from the source again. In every path, unchanged text hits the embedding/context caches and unchanged images hit the vision cache. |
| ST-5 | **Upload sources can't be re-fetched.** Only a *re-parse* (parser or transformer change) of an upload document needs the user to upload it again. The portal marks such documents "re-upload required" instead of silently keeping stale chunks. Chunk-size and contextual-retrieval changes apply to uploads without re-upload. |

## 7. Data model & migrations

| ID | Requirement |
|---|---|
| M-1 | Tables kept (semantics unchanged): `collections`, `collection_principals`, `sync_runs`, `api_keys`, `query_logs`. |
| M-2 | Changed: **`sources`**: `config` holds `connection_id` + target settings, with an FK to `connections` (`ON DELETE RESTRICT`). **`documents`**: drop `blob_key`, add `source_version text` (the connector's change marker), keep `content_hash` (hash of the parsed text, for skip-if-identical), add `last_error_at`. **`upload_items`**: drop `blob_key`, add `status` (`staged` / `indexed` / `failed` / `expired`), `scratch_node`. **`chunks`**: drop the `embedding` column; `text` holds **source text only** (the breadcrumb moves to `metadata.heading_path`); add `blocks jsonb` (block map: `[{offset, len, type, level?, page?, slide?}]`, a few bytes per block), `context text null` (§10.6), `embed_text_sha256`, `fts tsvector` (maintained by the indexer: title weight A, context weight B, text weight C, in the collection's language), with a GIN index. **`collections`**: `pipeline` JSONB gains the settings in I-8. |
| M-3 | **Embedding spaces** allow model changes without downtime: `embedding_spaces(id, connection_id, model, dim, status = building\|active\|retired, created_at, activated_at)` and `chunk_embeddings(chunk_id → chunks ON DELETE CASCADE, space_id, embedding vector)` (no fixed dimension on the column). Each space gets a **partial expression HNSW index**: `USING hnsw ((embedding::vector(<dim>)) vector_cosine_ops) WHERE space_id = <id>`. Exactly one space is `active`. Building a space fills it in the background from chunk text, mostly from the embedding cache. Activation is an atomic flip. Retired spaces are dropped after a grace period. |
| M-4 | New: `connections`, `platform_settings`, `audit_log`, `figures(id, document_id, collection_id, n, mime, width, height, data bytea)`, `embedding_cache`, `vision_cache`, `context_cache`, `jobs` (§9), `eval_sets`, `eval_items`, `eval_runs`, `eval_results`, `chat_feedback` (§12.3). |
| M-5 | The generated `tsv` column is replaced by the indexer-maintained `fts` column (M-2). The text-search language is a per-collection setting (`english` default, `simple` and other Postgres configs available). Indexes for filters: `documents(source_updated_at)`, `documents(mime_type)`, `documents(source_id)`. |
| M-6 | Status/role columns stay plain strings with app-level enums (Rust `enum` + `serde`), not Postgres enums. |
| M-7 | Migrations ship in the binary. Data migration from the Python schema is described in §17. |

## 8. Plugin model

Python entry points are replaced by a **compile-time registry**.

| ID | Requirement |
|---|---|
| P-1 | Traits: `Connector`, `Parser`, `Transformer`, `Chunker`, `Embedder`, `Vision`. All I/O is `async`. Parsing runs in parse worker processes (§10.4). |
| P-2 | Every plugin has a stable `name` and a typed config struct (`serde::Deserialize` + `JsonSchema`, `deny_unknown_fields`) validated at **onboarding time**, so the API call fails, not the nightly sync. |
| P-3 | Registration through `inventory` (or an explicit registry module). Each connector lives in its own crate in a Cargo workspace behind a Cargo feature, all on by default. |
| P-4 | **Connector contract: `list` + `fetch`.** `list(ctx) -> Stream<ItemRef>`, where `ItemRef = {external_id, title, uri, mime_type, source_version, size?, source_updated_at?, metadata, inline?}`, is cheap and metadata-only. `fetch(ctx, item) -> Stream<Bytes>` streams the content and is called **only in the index job**. Small text sources (Slack digests) may set `inline` content in `list` to avoid a second round-trip. |
| P-5 | A connector receives its **resolved connection** (decrypted secret) from the platform. It never reads env vars. `ready()` = connection exists, is enabled, and last test was OK (or untested). |
| P-6 | Connector metadata: `incremental`, `supports_url_targets`, `parse_target_url(url, &Connection)`, `connection_kind`. |
| P-7 | Parser auto-selection by MIME type with `priority`. An explicit `pipeline.parser` bypasses it. Parsers consume a **file path or a reader**, never a whole in-memory `Vec<u8>`, so memory stays flat for large files. |
| P-8 | `GET /plugins` returns plugins with their JSON Schemas, plus connection kinds with their config/secret schemas, so the portal generates forms. |
| P-9 | Third-party runtime plugins (WASM components or an out-of-process HTTP protocol) are **out of scope for v1**. The trait boundaries must not prevent them. |

### Built-ins to port

| Kind | Name | `list` / version marker | `fetch` |
|---|---|---|---|
| connector | `upload` | upload items in `staged` state; version = sha256 of the file | the scratch file (§10.3) |
| connector | `confluence` | **service-account auth via the `api.atlassian.com` gateway (§5.3)**; pages API (space / page / tree targets, URL-target parsing incl. short links); version = page `version.number` | storage-format body + attachment images inlined as data-URI `<img>` for vision, both through the gateway |
| connector | `sharepoint` | Graph drive walk or **delta query** (incremental cursor in `source.state`); version = `eTag`; extension filter, size cap | `@microsoft.graph.downloadUrl`, streamed |
| connector | `git` | **partial clone** (`--filter=blob:none`, `--depth 1`) in scratch, `git ls-tree -r`, include/exclude globs; version = blob SHA | `git cat-file blob <sha>` (fetches only the needed blobs) |
| connector | `slack` | daily digests per channel (inline text), lookback window; version = hash of digest | inline |
| parser | `text`, `markdown`, `html`, `tabular` | Same block types as today, with the fixes in §10.5. | |
| parser | `pdf`, `docx`, `pptx` | See §10.5. | |
| transformer | `basic-cleanup` | `drop_patterns`, `min_block_chars`, `collapse_whitespace`, `dedupe_repeated_blocks`. | |
| chunker | `default` | §11. | |
| embedder | `openai-compatible`, `fake` | Batching, bounded concurrency, retry with backoff on 429/5xx, timeout. `fake` is deterministic, for tests/dev only. | |
| vision | `openai-compatible`, `anthropic` | Same prompts and size limits as today. Empty description → block dropped. | |

## 9. Job queue & scheduling (replaces Procrastinate)

| ID | Requirement |
|---|---|
| Q-1 | `jobs` table: `id, queue, task, args jsonb, priority, status (todo/doing/succeeded/failed/cancelled), attempts, max_attempts, run_at, locked_by, locked_at, heartbeat_at, lock_key text null, pinned_node text null, last_error, created_at, finished_at`. |
| Q-2 | Workers claim with `SELECT … FOR UPDATE SKIP LOCKED` ordered by `priority, run_at`. `LISTEN/NOTIFY` wakes idle workers, with a polling fallback (≤ 5 s). |
| Q-3 | **Queueing lock**: partial unique index on `lock_key WHERE status IN ('todo','doing')`. A duplicate enqueue returns `AlreadyQueued`, which callers treat as success. Used for `sync:{collection}`, `index:{document}` and `reembed:{space}`. |
| Q-4 | **Transactional enqueue**: jobs are inserted in the same transaction as the rows they act on, so an index job can never run before its document row is visible. |
| Q-5 | Retries: sync 2, index 3, with exponential backoff. Exhausted → `failed` with `last_error`. Rate-limit errors (429) reschedule without consuming an attempt. |
| Q-6 | Stale-lock recovery: jobs whose `heartbeat_at` is older than N× the heartbeat interval are reset to `todo`. Pinned jobs whose node is gone are marked `failed` (§10.3). |
| Q-7 | **Fairness**: index workers round-robin across collections, so one 50k-document collection can't starve the others. |
| Q-8 | **Beat**: every minute, under a Postgres advisory lock, for each `sync_enabled` collection: arm `next_sync_at` if null, and if due, enqueue a sync and advance `next_sync_at` from the cron expression. Also runs nightly maintenance: cache pruning, job retention (14 days), retired-space cleanup. |
| Q-9 | `/analytics/db` job-status counts read from `jobs`. |

## 10. Ingestion pipeline

### 10.1 Sync (per collection): metadata only

For each **enabled** source:

1. Resolve the source's connection, and fail the source fast if it's missing, disabled or not usable.
2. Stream `list()` results. Commit in **batches of ~200 items**, so a failure late in a big space keeps earlier progress.
3. Per item: unseen → insert a `documents` row (`pending`) + an index job. `source_version` changed → update the row + an index job. Unchanged and `indexed` → skip **without downloading anything**. Unchanged and `failed` → requeue.
4. Full-enumeration connectors: after the listing completes successfully, tombstone documents not seen (cascade removes chunks, embeddings, figures). A listing that errors part-way **never** tombstones.
5. Persist `source.state` (cursor) and `last_synced_at`. Record per-source stats in `sync_runs`.
6. **Force resync** (`POST /collections/{id}/syncs {force: true}`) ignores `source_version` and enqueues every item.

### 10.2 Index (per document): fetch, parse, discard

`fetch → scratch file → parse (child process) → vision (cached) → transformer chain → chunk → embed (cached) → swap chunks atomically → delete scratch file`

| ID | Requirement |
|---|---|
| I-1 | Missing document row → error (retry). Status `deleted` → no-op. |
| I-2 | `fetch()` streams to `RAG_SCRATCH_DIR/{job_id}`, enforcing the per-source size cap while streaming. The file is deleted in all outcomes (RAII guard + startup sweep of the scratch dir). |
| I-3 | If the parsed text's `content_hash` equals the stored one (e.g. a Confluence version bump with no text change), only update metadata and skip chunking and embedding. |
| I-4 | Embedding input per chunk = `title + heading path + context sentence (if enabled, §10.6) + chunk text`. `embed_text_sha256` is looked up in `embedding_cache` for the active space (and any `building` space). Only misses go to the provider, in batches. |
| I-5 | Vision: `image` blocks are hashed and looked up in `vision_cache`. Misses go to the collection's vision model. Each image becomes `[Figure: …]`. Images below a minimum size (default 64×64) are skipped, with a per-document image cap (default 50). The figure image is stored only when `keep_figures` is on. Raw image bytes are never stored otherwise. |
| I-6 | Chunk replacement (delete old chunks → insert new chunks + embeddings + figures) happens in **one transaction**, using `COPY` for bulk insert. Until it commits, the **old chunks stay searchable**. |
| I-7 | On failure: the document keeps its previous chunks, `status = failed`, `error` is set (truncated to 4000 chars), and the job retries. |
| I-8 | Pipeline config (`collections.pipeline` JSONB), **edited per collection in the portal** (§11.1): `{parser?, transformers[] (default [basic-cleanup]), chunker {name: "default", max_chars: 1000, min_chars: 200, include_heading_path: true}, contextual {enabled, connection_id, model, scope: "window"\|"document", prompt?}, vision_llm? {connection_id, model, prompt?, min_image_px: 64, max_images: 50}, keep_figures (default false), fts_language (default "english")}`. New collections start from the platform defaults (admin-editable under *Settings → Indexing defaults*). |

### 10.3 Uploads

| ID | Requirement |
|---|---|
| U-1 | `POST …/files` streams the upload to scratch (size cap from platform settings, allow-list `.pdf .txt .md .markdown .csv .xlsx .docx .pptx`), computes sha256 while streaming, and inserts `upload_items` + `documents` + a **high-priority index job pinned to this node** in one transaction. |
| U-2 | Every node with the `api` role also runs an upload-only worker, so pinned jobs always have a consumer. |
| U-3 | After indexing, the scratch file is deleted and the item becomes `indexed`. If the node restarts before indexing, the item becomes `expired` and the UI shows "upload again". |
| U-4 | Deleting an upload item deletes its document (cascade removes chunks). |

### 10.4 Parse workers (child processes)

| ID | Requirement |
|---|---|
| W-1 | Parsing runs in a pool of child processes (`fable parse-worker`, size `RAG_WORKER__PARSE_PROCESSES`) talking over stdin/stdout (length-prefixed frames). The parent sends a scratch file path and parser config, and receives blocks as a stream. |
| W-2 | Per-parse limits: wall-clock timeout (default 300 s), memory limit (default 1 GB, via `setrlimit`), page limit (default 2,000). Exceeding a limit fails the document with a clear error; the worker is replaced. |
| W-3 | This isolates crashes in pdfium or malformed files, so the API never goes down because of a bad document. It also gives real parallelism, since pdfium is not thread-safe. |

### 10.5 Parsers

| ID | Requirement |
|---|---|
| X-1 | **PDF** (`pdfium-render`), page by page, constant memory. Text with font size and position. **Headings** detected by font size/weight relative to the page's body text median. **Headers/footers** removed when the same position band repeats across pages (digits normalised, so "Page 3 of 10" matches). Hyphenation repaired at line ends. `metadata.page` on every block. |
| X-2 | **PDF images** → `image` blocks when the collection has a vision model (size threshold and cap per I-5). |
| X-3 | **Scanned PDF pages** (no text layer): option `render_pages_without_text` renders the page to PNG as an `image` block, so the vision model acts as OCR. Default off. |
| X-4 | **DOCX**: `Heading1..9`/`Title` styles → `heading`, list paragraphs → `list_item`, tables → `table_row` as `header: value` pairs, `word/media/*` drawings → `image` blocks. Title from `docProps/core.xml`. |
| X-5 | **PPTX**: one heading per slide (title placeholder, or "Slide N"), text frames → paragraphs, tables → `table_row`, pictures → `image` blocks, speaker notes optional (default on), `metadata.slide`. |
| X-6 | **HTML / Confluence storage format**, fixing two defects in today's parser: the content of `ac:structured-macro name="code"` (`<![CDATA[…]]>` inside `ac:plain-text-body`) becomes a `code` block (today it's **dropped**); tables become `table_row` blocks with cells separated and paired with the header row (today cells are concatenated, e.g. `EnvURL`). Info/note/warning/expand macros keep their body text. |
| X-7 | **Tabular**: same row → `header: value` behaviour as today, `max_rows` cap. |
| X-8 | Docling config is rejected with a clear validation error. A migration rewrites `pipeline.parser.name == "docling"` to auto-select and logs which collections changed. |

### 10.6 Contextual retrieval

A chunk taken out of its document often doesn't say what it's about ("scale to 5 replicas if queue depth exceeds 10k": which service?). Contextual retrieval asks an LLM for **one sentence placing the chunk in its document**, and indexes that sentence alongside the chunk.

| ID | Requirement |
|---|---|
| CR-1 | Per-collection toggle (`pipeline.contextual.enabled`), **on by default for new collections** when a platform default model for it is configured. It uses a portal-managed LLM connection + model (a cheap model: Haiku-class, gpt-4o-mini-class or local Ollama). |
| CR-2 | Prompt input per chunk: document title, heading path, the chunk itself, and either its **neighbouring chunks** (`scope: "window"`, default; `window` = N chunks before and after, default 2, configurable 1–5, cache-stable when distant parts of the document change) or the **whole document** (`scope: "document"`, better for short documents, re-generated whenever the document changes). Output: one sentence, ≤ 50 tokens, stored in `chunks.context`. A configurable prompt has a sensible default. |
| CR-3 | The sentence is **included in the embedding input** (I-4) and in the full-text index (weight B, M-2). It is **not** shown as part of the chunk text in search results, chat citations or MCP output. It's returned as a separate `context` field. |
| CR-4 | `context_cache(key, model, prompt_sha256) → sentence`, where `key = sha256(title + heading path + chunk text + window text)` (or + document hash for `document` scope). Unchanged chunks never pay twice, including across re-chunks that don't touch them. |
| CR-5 | Calls are batched with bounded concurrency per connection. For `document` scope, Anthropic/OpenAI prompt caching is used so the document is sent once per document, not once per chunk. 429s reschedule without burning retries. |
| CR-6 | Failure policy: if the context model fails after retries, the chunk is indexed **without** context, and the document gets a warning (not `failed`), so search never loses content because of the enrichment step. The portal shows a per-collection "chunks missing context" count with a "retry context" action. |
| CR-7 | Turning it on for an existing collection, or changing its model/prompt/scope, runs a **re-context job** from stored chunks (no fetch, ST-4) with progress in the portal. Turning it off clears `context` and re-embeds. |
| CR-8 | The portal shows an estimated cost before enabling: chunk count × average tokens × the model's price, when a price is configured on the connection. |

## 11. Chunking

Port `DefaultChunker`; its tests are the spec, with the changes below.

| ID | Requirement |
|---|---|
| CH-1 | Non-overlapping: every source character lands in **exactly one** chunk. |
| CH-2 | **Size is measured in characters.** `max_chars` default **1000** (allowed range 200–8000); `min_chars` default **200** (0 disables). This replaces today's `max_tokens = 480` (≈ 1,900 characters), so expect roughly 2× as many chunks as today, which retrieve more precisely. |
| CH-3 | **Model safety cap**: whatever `max_chars` says, the full embedding input (title + path + context + chunk, I-4) is checked with the active embedding model's tokenizer (`tokenizers` crate) when known, or with `chars/4` otherwise. A chunk that would exceed the model's input limit is split further, so local 512-token models never truncate silently. The portal warns when `max_chars` is too high for the selected model. |
| CH-4 | A new chunk starts at every heading. The breadcrumb `A > B > C` is stored in `metadata.heading_path`. It is **not** stored in `text`; it's added to the embedding input and to displayed results when `include_heading_path` is on (default). |
| CH-5 | Sections smaller than `min_chars` merge with the following section (keeping the first section's breadcrumb), so a heading followed by one line doesn't become its own chunk. |
| CH-6 | Oversized blocks split on sentence ends, with a hard split only for single sentences over the limit. |
| CH-7 | Figure refs roll up into `metadata.figures`. |
| CH-8 | **Block map**: each chunk stores `blocks` (offset, length, type, heading level, page/slide) for the blocks it contains. **Re-chunking** (a chunker setting change) rebuilds the document's block list from its stored chunks + block maps + heading paths in ordinal order and runs the chunker again. **No fetch or re-upload.** A test asserts that re-chunking from stored chunks gives the same result as chunking the original parse. |

### 11.1 Per-collection indexing settings in the portal

| ID | Requirement |
|---|---|
| PS-1 | Each collection has an **Indexing settings** panel (collection `owner` and `editor`; admins everywhere) covering every I-8 field. *Chunking*: max/min characters, heading path. *Contextual retrieval*: on/off, connection, model, scope, prompt. *Vision*: connection, model, min image size, max images, keep figures. *Parsing*: parser (auto or a specific one), cleanup options. *Search*: full-text language. |
| PS-2 | Forms are generated from the plugins' JSON Schemas (P-8), with server-side validation (422 with a field-level message). |
| PS-3 | **Impact preview before saving**: the panel says what each change triggers and for how many documents/chunks: *re-embed* / *re-context* / *re-chunk* (no fetch, minutes) or *re-parse* (force resync from sources; upload documents need re-upload), plus estimated LLM cost where relevant (CR-8). Saving queues the needed job automatically; progress shows on the collection page. |
| PS-4 | Until the rebuild finishes, search keeps serving the previous chunks (I-6). Each document switches atomically when its own rebuild completes. |
| PS-5 | Settings changes are recorded in the audit log (S-10). |
| PS-6 | `PATCH /collections/{id}` accepts `pipeline` changes. `POST /collections/{id}/pipeline/preview` returns the impact preview without saving. |

## 12. Retrieval

### 12.1 Ranking & access

| ID | Requirement |
|---|---|
| R-1 | **ACL first**: resolve readable collection ids for the principal, then hard-filter SQL with `collection_id = ANY($1)`. Forbidden chunks are never scored. |
| R-2 | **Visibility fix**: a document is searchable when it has chunks and `status <> 'deleted'`. `pending` or `failed` documents keep serving their last good chunks. (Today, changed documents vanish from search until reindexed.) |
| R-3 | **Filtered ANN fix**: vector queries run with `SET LOCAL hnsw.iterative_scan = relaxed_order` and `hnsw.ef_search ≥ fetch size`, so a search over a small collection inside a large index still returns `top_k` results. (Today it can return fewer, or none.) |
| R-4 | `vector` mode: cosine on the **active** embedding space, score `1 - distance`. |
| R-5 | `hybrid` mode (default): vector (fetch `4×top_k`) + full-text `websearch_to_tsquery(<collection language>, q)` over the `fts` column (title A, context B, text C) with `ts_rank` (fetch `4×top_k`), fused by RRF with k=60. Hits return `text`, `heading_path` and `context` as separate fields. |
| R-6 | `top_k` clamped to the platform `search_max_top_k` (default 50). |
| R-7 | Every `/search` and `/chat` writes a `query_logs` row (principal, auth method, key id, query ≤ 4000 chars, mode, top_k, collection ids, hit count, latency). |
| R-8 | Shared HTTP client for embedder/vision/chat. Short-TTL (30 s) cache of principal → readable collection ids, invalidated on ACL change. Query-embedding cache via `embedding_cache`. |

### 12.2 Search filters

| ID | Requirement |
|---|---|
| F-1 | `POST /search`, `POST /chat` and the MCP `search_documents` tool accept an optional `filters` object: `source_ids[]`, `connectors[]` (e.g. `confluence`, `slack`), `mime_types[]` (exact or prefix like `application/pdf`, `text/*`), `updated_after` / `updated_before` (ISO-8601 dates against `documents.source_updated_at`, falling back to `indexed_at` when the source gives no date), `title_contains`. All fields optional; combined with AND, values within a field with OR. |
| F-2 | Filters are applied **inside** the ACL-filtered SQL of both search legs (vector and full-text), never as post-filtering of results, so `top_k` stays complete (with R-3 iterative scans). |
| F-3 | Unknown or inaccessible `source_ids` behave like inaccessible collections (no existence leak); invalid values → 422. |
| F-4 | MCP tool schema and description document the filters with examples (e.g. "use `updated_after` for questions about recent changes"), so agents use them on their own. Existing MCP clients that don't send filters keep working unchanged. |
| F-5 | `GET /search/facets?collection_ids=` returns, for the caller's readable scope, the available sources, connectors and MIME types with document counts, so the portal search page and MCP clients can offer filter choices. |
| F-6 | `query_logs` records the filters used (jsonb), for analytics and evaluation. |

### 12.3 Retrieval evaluation & feedback

Every change to chunking, parsing, models or contextual retrieval must be measurable, not guessed.

| ID | Requirement |
|---|---|
| E-1 | **Golden sets**: `eval_sets(id, name, collection_ids[], created_by)` and `eval_items(id, set_id, question, expected_document_ids[], expected_chunk_ids[]?, notes)`. Managed in the portal (*Evaluation* page per collection; owners/editors) and importable/exportable as YAML/JSON. |
| E-2 | **Seeding**: the portal suggests candidate questions from `query_logs` (frequent queries) and from thumbs-down chat feedback (E-6). A person picks the expected documents from the search results with one click. |
| E-3 | **Runs**: `fable eval --set <name> [--k 1,5,10] [--mode hybrid\|vector] [--json]` (CLI), `POST /eval-sets/{id}/runs` (API) and a "Run evaluation" button. Each run stores `eval_runs(id, set_id, started_at, config_snapshot jsonb, metrics jsonb)` and per-question `eval_results(run_id, item_id, rank_of_first_expected, hit_ids[])`. Runs execute through the same search code path as users, under the requester's ACL. |
| E-4 | **Metrics**: recall@1/5/10 (share of questions whose expected document appears in the top k), MRR (mean of 1/rank of the first expected hit, 0 if absent), plus per-question pass/fail. The config snapshot records the pipeline settings, embedding space, contextual on/off and filters, so runs are comparable. |
| E-5 | **Comparison**: the portal shows run history per set and a diff between two runs: metric deltas, plus questions that got better or worse with both result lists side by side. |
| E-6 | **Chat feedback**: thumbs up/down (+ optional comment) on each chat answer, stored in `chat_feedback(id, query_log_id, principal_oid, rating, comment, retrieved_chunk_ids[], answer_text, created_at)`. Visible to collection owners/editors for their collections and to admins platform-wide. A thumbs-down can be turned into a golden item in one click. |
| E-7 | **Regression gate in CI**: a fixture corpus + golden set lives in the repo. CI runs `fable eval` with the `fake` embedder for determinism checks, and an optional nightly job runs with real models. A drop beyond a configured threshold (e.g. recall@5 −0.05) fails the job. |
| E-8 | **Pipeline-change guardrail**: the impact preview (PS-3) links to "run the evaluation set on this collection after the rebuild", and the collection page shows the latest eval metrics next to the settings that produced them. |

## 13. API surface

Existing routes keep their paths, status codes and response shapes, except where this document changes the underlying model: source config (`connection_id` instead of `*_env` fields), figures, and model settings. The UI and existing API-key/MCP clients must keep working. OpenAPI is served (`utoipa`) at `/openapi.json` with Swagger UI at `/docs`.

| Area | Routes |
|---|---|
| Health | `GET /healthz`, `GET /readyz` (DB probe) |
| UI | `GET /` → redirect `/ui/`, `GET /ui/*` (embedded static, `Cache-Control: no-cache`), `GET /ui-config` (public), `GET /me` |
| Collections | `POST/GET /collections`, `GET/PATCH/DELETE /collections/{id}`, `GET/PUT /collections/{id}/permissions` |
| Sources | `POST /collections/{id}/sources/test`, `POST/GET /collections/{id}/sources`, `PATCH/DELETE …/sources/{sid}`, `GET …/sources/{sid}/documents` |
| Uploads | `POST/GET …/sources/{sid}/files` (streaming multipart), `DELETE …/files/{item}` |
| Documents | `GET /collections/{id}/documents` (status filter, `q`, paging), `GET …/documents/{doc}`, `GET …/documents/{doc}/chunks` |
| Syncs | `POST /collections/{id}/syncs` (202, queueing-locked, body `{force?: bool}`), `GET /collections/{id}/syncs` |
| Search | `POST /search` (optional `filters`, §12.2), `GET /search/collections`, `GET /search/facets`, `GET /documents/{doc}/chunks` (paged, `from_ordinal`, `limit` ≤ 100, `total`), `GET /search/figures/{figure_id}` (ACL-checked; 404 when figures aren't kept) |
| Chat | `POST /chat` (SSO only, optional `filters`, 503 when disabled, 502 on model error), `POST /chat/{query_log_id}/feedback` (thumbs up/down + comment), `GET /collections/{id}/feedback` |
| Indexing settings | `PATCH /collections/{id}` (`pipeline`), `POST /collections/{id}/pipeline/preview`, `GET /collections/{id}/rebuilds` (progress of re-embed / re-context / re-chunk / resync jobs) |
| Evaluation | `GET/POST /eval-sets`, `GET/PATCH/DELETE /eval-sets/{id}`, `GET/POST /eval-sets/{id}/items`, `PATCH/DELETE /eval-items/{id}`, `POST /eval-sets/{id}/import`, `GET /eval-sets/{id}/export`, `POST /eval-sets/{id}/runs`, `GET /eval-sets/{id}/runs`, `GET /eval-runs/{id}`, `GET /eval-runs/{a}/compare/{b}`, `GET /collections/{id}/eval-suggestions` |
| Plugins | `GET /plugins` (plugins + connection kinds with schemas) |
| API keys | `POST /api-keys` (secret shown once), `GET /api-keys`, `DELETE /api-keys/{id}` |
| Analytics | `GET /analytics/db`, `GET /analytics/queries?days=14` |
| **Admin: connections** | `GET/POST /admin/connections`, `GET/PATCH/DELETE /admin/connections/{id}`, `POST /admin/connections/{id}/test`, `POST /admin/connections/test` (unsaved), `GET /admin/connections/{id}/usage` |
| **Connections (non-admin)** | `GET /connections?kind=` (only connections usable by the caller; id, name, kind, no config details) |
| **Admin: settings** | `GET/PUT /admin/settings/embedding`, `GET/PUT /admin/settings/chat`, `GET/PUT /admin/settings/vision`, `GET/PUT /admin/settings/limits` |
| **Admin: embedding spaces** | `GET /admin/embedding-spaces`, `POST /admin/embedding-spaces` (start building), `POST /admin/embedding-spaces/{id}/activate`, `DELETE /admin/embedding-spaces/{id}` |
| **Admin: audit** | `GET /admin/audit-log` (paged, filterable) |

Failure-shape rules: **no access → 404** (existence not leaked), **insufficient role → 403**, validation → 422, unsupported upload type → 415, too large → 413, dependency conflict → 409.

## 14. AuthN / AuthZ

| ID | Requirement |
|---|---|
| A-1 | Pure OAuth2 resource server for Entra ID v2: validate signature (JWKS), `iss = https://login.microsoftonline.com/{tenant}/v2.0`, `aud`, `exp`/`nbf` with small leeway. |
| A-2 | Principal: `oid`, `name`, `email`, `groups` claim → `group_ids`, `roles` claim mapped through `role_map` (default `Rag.Admin→admin`, `Rag.User→user`), `default_role` when nothing maps. |
| A-3 | **API keys**: `Bearer ragk_…`, SHA-256 lookup, reject revoked or expired keys, principal = owner oid with no groups and `key_scopes`, search-only routes (`/search`, `/search/collections`, `/documents/{id}/chunks`, `/search/figures/*`, `/mcp`). Throttled `last_used_at`. Keys can't mint keys, create collections or reach `/admin/*`. |
| A-4 | RBAC as pure, unit-tested functions: `effective_collection_role`, `require_collection_role`, `can_create_collections`, plus `can_use_connection(principal, connection)` (§5 S-7). |
| A-5 | Key access = key scopes ∩ owner's *current* direct access. |
| A-6 | `disabled` mode injects a dev admin principal, and only outside prod. |

## 15. MCP

| ID | Requirement |
|---|---|
| MCP-1 | Streamable-HTTP MCP server at `/mcp` exposing three tools: `search_documents`, `list_searchable_collections`, `get_document_chunks`. |
| MCP-2 | Tool names and descriptions (incl. the paging guidance text) match today's. Input schemas are today's plus the **additive** `filters` parameter on `search_documents` (§12.2 F-4), and hits gain `heading_path` and `context` fields. Existing clients keep working. |
| MCP-3 | The caller's `Authorization` header (Entra token or API key) authenticates, with the same ACL as REST. Tools call the same service functions as REST handlers. |
| MCP-4 | OAuth discovery metadata as described in `docs/MCP_OAUTH.md` is preserved. |

## 16. Non-functional

| ID | Requirement |
|---|---|
| N-1 | Image < 150 MB. Idle RSS < 100 MB (excluding parse workers). Cold start to `/readyz` OK < 2 s (excluding migrations). |
| N-2 | Search p50 overhead (excluding the embedding call) ≤ the Python build's on a comparable DB. |
| N-3 | Parsing a 100-page text PDF < 2 s without vision. Peak memory of a parse worker is independent of file size for PDF, DOCX and PPTX (page/part streaming). |
| N-4 | Storage: after indexing, durable storage per document is chunk text + vectors + one document row (+ figures if enabled). Verified by a test over a fixture corpus: DB growth ≤ 1.5× chunk text + vectors. |
| N-5 | Secrets never appear in logs, traces, error messages, API responses or audit diffs. Enforced by a test that greps captured logs for a known secret. |
| N-6 | Structured JSON logs with principal/document/job context and request IDs. |
| N-7 | No `unsafe` in our crates (deps excepted). `cargo clippy -D warnings`, `cargo fmt --check`, `cargo deny` in CI. |
| N-8 | linux/amd64 and linux/arm64. |

## 17. Testing & cutover

| ID | Requirement |
|---|---|
| T-1 | Unit tests: chunker invariants (non-overlap, `min_chars`/`max_chars`, model safety cap, re-chunk-from-stored equals chunk-from-parse), contextual retrieval (prompt assembly per scope, cache keys, failure → indexed without context), filter SQL building (each field, AND/OR semantics, no existence leak), eval metrics (recall@k/MRR on hand-computed cases), RBAC matrix incl. `can_use_connection`, parsers (fixtures per format incl. Confluence code macro + tables, PDF headings/footers), transformers, secret encryption round-trip and rotation, embedder batching and retry, cache hit paths, Confluence auth (OAuth token fetch/cache/refresh-on-401, scoped-token Bearer, gateway URL building, `_links.next` rebasing, no request ever sent to `*.atlassian.net` with credentials) against a mock gateway. |
| T-2 | Integration tests against real Postgres (`testcontainers`): migrations, queue semantics (SKIP LOCKED, queueing lock, pinned jobs, stale recovery), sync → index → search end-to-end with the `fake` embedder, blue/green re-embed, "no payload persisted" (ST-1), filtered ANN returns `top_k` on a small collection inside a large index. |
| T-3 | **API contract suite**: an HTTP test suite covering every route in §13, run against the Rust build. Routes unchanged from today are also run against the Python build to confirm their shapes match. |
| T-4 | **Cutover (one-time, in place):** (1) back up the DB; (2) stop the Python containers; (3) start the Rust image: migrations convert the schema, the env import (C-4) creates connections and model settings, existing `chunks.embedding` values move into an initial embedding space (**no re-embedding, no re-fetch**), and `blob_key` columns are dropped; (4) verify with the contract suite; (5) delete the old `appdata` volume. Uploaded documents keep their chunks; only a later pipeline change would need re-upload (ST-5). |
| T-5 | CI: `cargo test`, clippy, fmt, `sqlx prepare --check`, `docker build` (multi-arch), contract suite. |

## 18. Open decisions

1. **Keep originals for upload sources (opt-in)?** It's the only way to re-parse uploads without users re-uploading. Recommendation: no in v1; revisit if pipeline changes on upload-heavy collections become common.
2. **Figures default.** Off (index the description only) vs on (show the image in search results, ~50 KB each). Recommendation: off by default, a per-collection toggle.
3. **Master key source.** Env var only, or also a KMS/Key Vault integration? Recommendation: env var in v1, with the `key_version` column leaving room for KMS later.
   **Decided (FAB-142):** v1 reads the master key only from `RAG_MASTER_KEY` (32 random bytes, base64), parsed in the `fable-core` bootstrap config; it is required outside `dev`, and in `dev` an auto-generated key is stored in the database with a loud warning (C-7). No KMS/Key Vault in v1. Every encrypted row stores `key_version` (AES-256-GCM, S-4); key lookup goes through the `MasterKeyProvider` seam resolved by `key_version`, so a KMS/Key Vault provider can be added later without a schema change. Implemented in FAB-41: the `master_key_state` row holds the current `key_version` and a key-check value, so startup fails with a clear message on a missing or wrong key while secrets exist; `fable rotate-master-key --new-key-file <file>` (or `--new-key <base64>`) re-encrypts every secret in one transaction and is audited. Rotation procedure: stop the app, run the rotation with the current `RAG_MASTER_KEY`, set `RAG_MASTER_KEY` to the new key, start the app. In `dev`, running it without `RAG_MASTER_KEY` moves secrets off the generated dev key.
4. **Git connector**: `git` CLI (parity, partial clone support) vs `gix` (pure Rust). Recommendation: `git` CLI in v1.
5. **Workspace layout**: Cargo workspace (`fable-core`, `fable-api`, `fable-worker`, `connector-*`, `fable` bin) mirroring today's packages, one binary. Recommendation: yes.
6. **Portal front-end stack**: keep the no-build vanilla-JS SPA, or adopt a component framework (e.g. React or Svelte + TypeScript + Vite) built in the Docker builder stage and embedded with `rust-embed`. Recommendation: adopt a framework. The §20 scope (data tables, schema-driven forms, admin area, accessibility) is a lot to hand-roll, and the build stays inside the image build, so the single-container goal is unaffected.

## 19. Later improvements (not v1)

From the indexing evaluation, deferred so v1 stays focused. Contextual retrieval (§10.6), search filters (§12.2) and retrieval evaluation (§12.3) were moved into v1.

- Cross-encoder **reranker** in-process (`fastembed-rs`, ONNX on CPU) over ~30 fused candidates, measured with §12.3 before being turned on by default.
- **Slack**: one document per thread, user IDs resolved to names.

## 20. Portal UI / UX

The portal is the only place the platform is configured, so it has to be clear, fast and hold up at real-world scale. Today's UI is dark-themed, unpaginated in most lists (e.g. every collection is rendered as a checkbox on the search page), and has no admin area.

### 20.1 Design system

| ID | Requirement |
|---|---|
| UX-1 | **Light theme by default.** Design tokens (colour, type, spacing, radius, elevation) as CSS variables; an optional dark theme follows `prefers-color-scheme` with a user toggle. All text and controls meet **WCAG 2.1 AA** contrast in both themes. |
| UX-2 | **App shell**: left sidebar navigation (collapsible), top bar with global search / command palette (`Ctrl/Cmd+K`), signed-in user and role, breadcrumbs on every page. Navigation is role-aware: the **Admin** section appears only for admins (and is enforced server-side). |
| UX-3 | **Component library** used everywhere: buttons, inputs, selects/combobox, toggles, badges/status pills, tabs, modals and side drawers, toasts, tooltips, confirm dialogs for destructive actions (type-to-confirm for deleting collections/connections), skeleton loaders, empty states with a next action, inline error states with retry. |
| UX-4 | **Schema-driven forms** (connections, plugins, indexing settings) with inline validation from the API's field-level 422s, help text and examples per field, write-only secret fields ("set · updated 3 days ago · Replace"), unsaved-changes guard. |
| UX-5 | **Accessibility**: full keyboard operation, visible focus, ARIA roles and labels, screen-reader-friendly status updates for long-running jobs; checked with an automated audit (axe) in CI plus a manual pass. |
| UX-6 | **Responsive**: optimised for desktop (≥ 1280 px), fully usable on tablet; on phones, search and chat work and admin pages are readable. |
| UX-7 | **Feedback for async work**: syncs, rebuilds, re-embeds and eval runs show live progress (counts, ETA, failures) without page reloads, and finished jobs raise a toast with a link to details. |

### 20.2 Lists that scale

| ID | Requirement |
|---|---|
| UX-8 | **Every list endpoint is server-side paginated, sortable and filterable** (collections, sources, documents, syncs, API keys, connections, audit log, jobs, eval items, feedback, query logs) with consistent `limit`/`cursor` (or `offset`), `sort`, `q` parameters and a total count. No endpoint returns an unbounded list. |
| UX-9 | **Reusable data table**: sortable columns, filter bar, debounced search, pagination with page-size choice, sticky header, column visibility, row actions menu, bulk select + bulk actions (e.g. retry failed documents, delete), and status chips. State (filters, sort, page) is kept in the URL so views are linkable and survive reload. |
| UX-10 | **Scalable collection picker** replaces checkbox lists (search, chat, API-key scopes, eval sets): typeahead search, multi-select chips, "recent" and "favourites", grouped by visibility/owner, "all readable collections" option. Must stay fast with 1,000+ collections. |
| UX-11 | **Long views** (documents in a large collection, chunks of a long document, audit log) use pagination or virtualised scrolling; never render thousands of rows at once. |
| UX-12 | **Performance budget**: with a seed of 1,000 collections, 100k documents and 2M chunks, every list page shows its first page in < 500 ms (API p95) and stays responsive while scrolling or filtering. Checked by a seeded perf test. |

### 20.3 Information architecture

| Area | Pages |
|---|---|
| **Workspace** (all users) | Home (my collections, recent searches, system notices) · Search · Chat · Collections (list → detail tabs: *Overview*, *Sources*, *Documents*, *Syncs*, *Indexing settings*, *Access*, *Evaluation*, *Feedback*) · API keys · Docs |
| **Admin** (admins only) | Overview (health, queue depth, failing syncs, connections needing attention, expiring credentials, model usage) · **Integrations** (Confluence, SharePoint, Slack, Git connections) · **Model providers** (OpenAI, Azure OpenAI, Anthropic, Ollama, LiteLLM, OpenAI-compatible) · **Model roles** (Embedding, Chat, Vision, Context line, with test panels) · Indexing defaults · Limits · Collections (all collections, owners, size, transfer ownership) · Access (admins, API keys platform-wide, revoke) · Jobs (queue monitor: running/failed, retry/cancel) · Audit log |

### 20.4 UX quality bar

| ID | Requirement |
|---|---|
| UX-13 | Key flows are designed (wireframes → hi-fi) and reviewed **before** they're built: first-run setup (configure a model provider → assign roles → add an integration → create a collection → add a source → first search), adding a Confluence source by pasting a link, changing indexing settings with the impact preview, and running an evaluation. |
| UX-14 | **First-run setup wizard** for a fresh install: the admin is guided through model providers, model roles (at least Embedding) and one integration before the workspace is usable; each step has a "test" check. |
| UX-15 | A short **usability test** (3–5 people, the flows in UX-13) is run before cutover; findings are fixed or ticketed. |
| UX-16 | Visual regression tests (screenshots of key pages in both themes) run in CI. |
