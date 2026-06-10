# RAG Platform

Extensible, plugin-driven enterprise RAG platform. Python (uv workspace) + PostgreSQL — and nothing else: pgvector for vector search, Postgres FTS for the lexical leg of hybrid retrieval, and [Procrastinate](https://procrastinate.readthedocs.io/) as the Postgres-native job queue (no Redis/RabbitMQ).

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full design.

## What it does

- **Collections** group documents and carry their own indexing pipeline config, cron sync schedule, and ACL.
- **Sources** are onboarded into a collection via connector plugins: SharePoint, Confluence (space / page / tree / folder), Git repos, Slack channels, and manual uploads (pdf, txt, md, csv, xlsx).
- **Cron sync** per collection: a worker beat evaluates each collection's cron expression and runs delta syncs (content-hash based — unchanged documents are skipped, vanished ones are tombstoned).
- **Indexing pipeline** per document: parse → transformer chain (cleanup/enrichment plugins) → chunking (**strictly non-overlapping**, structure-aware) → embed → pgvector.
- **Retrieval service**: `POST /search` with vector or hybrid (vector + FTS, RRF-fused) modes, hard-filtered to collections the caller can read.
- **Auth**: OAuth2 resource server validating Microsoft Entra ID bearer tokens. Two global roles from Entra app roles — `Rag.Admin` and `Rag.User` — plus per-collection ACLs for Entra users and groups (`owner` / `editor` / `viewer`). Collections are `private` (ACL only) or `public` (everyone gets viewer). Any user can create collections and becomes their owner.
- **API keys**: any signed-in user can mint `ragk_…` keys scoped to collections they can read (the UI offers exactly that list). Keys are search-only — every management endpoint rejects them — and are stored hashed, shown once, revocable, optionally expiring.
- **Management UI** at `/ui/`: MSAL (auth-code + PKCE) sign-in, dashboard with database analytics (documents, chunks, storage, queue, sync runs) and query analytics (volume, latency, top/recent queries — every `/search` is logged), collection/source/upload management, ACL editor, API key management. Admins see platform-wide analytics; users see their own.

## Repository layout

```
packages/
  rag-core/      domain model, plugin contracts, built-in plugins, pipeline, retrieval
  rag-api/       FastAPI app: Entra ID auth, RBAC, management + search endpoints
  rag-worker/    Procrastinate worker: cron beat, sync engine, indexing tasks
plugins/
  rag-connector-confluence/   each connector is its own installable package,
  rag-connector-sharepoint/   registered via the `rag.connectors` entry point —
  rag-connector-git/          the template for writing your own
  rag-connector-slack/
migrations/      Alembic (schema) — procrastinate schema applied via `rag-apply-schema`
tests/           unit tests (no database required)
```

## Quickstart (local dev)

```sh
# Everything in containers (db + migrations + api + worker), auth disabled, fake embedder:
docker compose up --build
# API at http://localhost:8000/docs

# Or natively (Postgres from compose only):
uv sync
docker compose up -d db
uv run alembic upgrade head
uv run rag-apply-schema
uv run uvicorn rag_api.main:app --reload     # terminal 1
uv run rag-worker                            # terminal 2
```

Copy `.env.example` to `.env` and adjust. Dev defaults: `RAG_AUTH__MODE=disabled` (injects a local admin principal; refused when `RAG_ENVIRONMENT=prod`) and the deterministic `fake` embedder. For production set `RAG_AUTH__MODE=entra`, the tenant/audience, and a real embedder.

```sh
uv run pytest          # tests
uv run ruff check .    # lint
```

## Try the full loop

```sh
# 1. Create a collection (default pipeline, 2 AM daily sync)
curl -X POST localhost:8000/collections -H 'content-type: application/json' \
  -d '{"name": "handbook", "sync_cron": "0 2 * * *"}'

# 2. Add a manual-upload source     (use the id from step 1)
curl -X POST localhost:8000/collections/$COLL/sources -H 'content-type: application/json' \
  -d '{"name": "files", "connector": "upload"}'

# 3. Upload a document              (ids from steps 1-2)
curl -X POST localhost:8000/collections/$COLL/sources/$SRC/files -F file=@handbook.pdf

# 4. Sync now instead of waiting for cron
curl -X POST localhost:8000/collections/$COLL/syncs

# 5. Search
curl -X POST localhost:8000/search -H 'content-type: application/json' \
  -d '{"query": "vacation policy", "mode": "hybrid"}'
```

## Writing a plugin

Any pip-installable package can extend the platform — no core changes. Implement one of the ABCs in `rag_core.interfaces` and register it under the matching entry-point group:

| Kind | Group | Contract |
|---|---|---|
| connector | `rag.connectors` | yield `RawDocument`s from an external system |
| parser | `rag.parsers` | bytes/mime → structured `ParsedDocument` |
| transformer | `rag.transformers` | cleanup/enrichment between parsing and chunking |
| chunker | `rag.chunkers` | `ParsedDocument` → non-overlapping chunks |
| embedder | `rag.embedders` | texts → vectors |

```toml
# your-plugin/pyproject.toml
[project.entry-points."rag.connectors"]
jira = "my_pkg.connector:JiraConnector"
```

Each plugin declares a pydantic `config_model`; operator-supplied config is validated at onboarding time. Convention: configs never contain secrets — they name environment variables (`api_token_env`) that hold them. `GET /plugins` lists everything installed with JSON schemas. The packages under `plugins/` are complete working examples.

## Entra ID setup (production)

1. App registration for the API; expose it (Application ID URI → `RAG_AUTH__AUDIENCE`).
2. Define app roles `Rag.Admin` and `Rag.User`; assign to users/groups on the enterprise application. Users without an app role get `RAG_AUTH__DEFAULT_ROLE` (default `user`; set to empty to require explicit assignment).
3. Enable **group claims** (security groups) in the token configuration so group-based collection ACLs work.
4. Second app registration of type **SPA** for the management UI (`RAG_AUTH__SPA_CLIENT_ID`), redirect URI `https://<host>/ui/`, granted the API's delegated scope. The UI signs in with MSAL via auth-code + PKCE.
5. Services use client credentials — or simpler, a scoped platform API key (`Authorization: Bearer ragk_…`).
