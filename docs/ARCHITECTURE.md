# Architecture

## Goals and constraints

- Enterprise RAG platform, **extensible via plugins** as the primary design axis.
- Microsoft Entra ID OAuth with real RBAC from day one.
- Collections onboard heterogeneous sources (SharePoint, Confluence, Git, Slack, manual uploads) and sync them on per-collection cron schedules.
- Indexing pipeline with pluggable cleanup/transformation; **non-overlapping chunking**.
- Stack: Python managed by uv, PostgreSQL as the *only* stateful infrastructure.

## System shape

```
                 ┌────────────────────────────────────────────────┐
   Entra ID ──── │  rag-api (FastAPI)                             │
   (JWT, JWKS)   │  authn → RBAC → collections/sources/uploads/   │
                 │  search; defers jobs by task name              │
                 └────────────┬───────────────────────────────────┘
                              │ PostgreSQL (single instance)
                              │  • relational model  • pgvector chunks
                              │  • tsvector FTS      • procrastinate queue
                 ┌────────────┴───────────────────────────────────┐
                 │  rag-worker (Procrastinate)                    │
                 │  beat (1 min) → due cron syncs → sync engine   │
                 │  → connector plugins → blob store → index jobs │
                 │  → parse/transform/chunk/embed → chunks        │
                 └────────────────────────────────────────────────┘
```

Two deployable processes (API, worker) from one image; both scale horizontally. Postgres carries data, vectors, full-text and the job queue — Procrastinate was chosen over Celery specifically to avoid a second broker.

## Monorepo (uv workspace)

`packages/rag-core` is the contract layer: every other package depends only on it. `rag-api` and `rag-worker` are thin process shells. Each connector under `plugins/` is an independent wheel — proof that third-party extension works, and the template for it. The API defers jobs through `rag_core.queue` *by task name*, so the API never imports worker code.

## Plugin model

Discovery via Python entry points — install a package, restart, it's there. Five extension kinds (groups `rag.connectors`, `rag.parsers`, `rag.transformers`, `rag.chunkers`, `rag.embedders`), each a small ABC in `rag_core.interfaces` with a pydantic `config_model` validated at onboarding time (bad config fails the API call, not the nightly sync). Built-ins register through the same mechanism, so there is exactly one code path.

Per-collection pipeline config (JSONB): optional pinned parser (else auto-selected by mime type), ordered transformer chain, chunker. The **embedder is platform-global** — one vector space, one column dimension; per-collection embedders would break cross-collection search and the HNSW index. Changing it is a migration + reindex, by design.

Secrets convention: plugin configs store *names of environment variables* (`api_token_env`), never values. Swap in a vault-backed resolver later without touching connectors.

## Data model

```
collections (pipeline JSONB, sync_cron, next_sync_at)
 ├── collection_principals    ACL: (user|group Entra oid) → owner|editor|viewer
 ├── sources                  connector name + validated config JSONB + cursor state
 │    ├── upload_items        staged manual uploads (soft-deleted)
 │    └── documents           external_id, content_hash, blob_key, status
 │         └── chunks         text, metadata, vector(D), generated tsvector
 └── sync_runs                audit trail with per-source stats
```

Delta detection is content-hash based (`sha256` of payload), uniform across connectors. Raw payloads land in a blob store (local FS now, `BlobStore` ABC for S3/Azure later) so re-indexing — e.g. after a pipeline config change — never re-fetches from the remote system.

## Sync & scheduling

Procrastinate periodic task fires every minute, compares `collections.next_sync_at` to now, enqueues due syncs and advances `next_sync_at` via croniter — exactly-once per tick, restart-safe, no scheduler process. Manual syncs use the same task with a `queueing_lock` so a collection never runs two syncs concurrently.

The sync engine iterates each enabled source in its own transaction (one bad source can't poison the run), upserts changed documents, enqueues per-document index jobs, and tombstones documents the connector stopped yielding (full-enumeration connectors; incremental connectors opt out via `incremental = True` and manage state in `source.state`).

## Chunking (non-overlapping)

`DefaultChunker` packs structural blocks (headings/paragraphs/list items/table rows from the parser) into chunks up to a token budget, breaking at every heading. Every source character lands in **exactly one chunk** — enforced by tests. Heading breadcrumbs (`Guide > Setup > Auth`) are prepended as context, which restores most of what overlap usually buys without duplicating index content. Oversized blocks split at sentence boundaries.

## Retrieval

`POST /search`: embeds the query, pgvector cosine ANN (HNSW) over the caller's readable collections; hybrid mode adds Postgres FTS (`websearch_to_tsquery` over a generated tsvector column) and fuses with Reciprocal Rank Fusion. ACL filtering is done by resolving readable collection ids *first* and hard-filtering the SQL — chunks of forbidden collections are never scored.

## AuthN / AuthZ

API is a pure OAuth2 resource server: clients bring Entra ID bearer tokens; the API validates signature (JWKS, cached), issuer, audience. No token issuance, no sessions, no secrets for interactive auth.

Two-layer RBAC, enforced in `rag_core.rbac` (pure functions, unit-tested):

1. **Global roles** mapped from Entra app roles: `admin` (everything), `curator` (create collections), `reader` (API access; per-collection rights only).
2. **Collection ACLs**: rows granting Entra users or **groups** `owner` / `editor` / `viewer`. Owner manages ACLs; editor manages sources/uploads/syncs; viewer searches. Creator becomes owner; admins are implicit owners. Group membership comes from the token's `groups` claim.

Failure shape is deliberate: no access → 404 (existence not leaked); insufficient role → 403. `RAG_AUTH__MODE=disabled` provides a dev principal and hard-fails in prod.

## Operations

- **Migrations**: Alembic (autogenerate-ready: naming conventions, metadata wired) + `rag-apply-schema` for the queue tables; compose runs both before app start.
- **Observability**: structlog JSON logs with principal/document context binding; `/healthz` (liveness) and `/readyz` (DB probe). Add OTel + Prometheus exporters as a follow-up.
- **CI**: ruff (lint+format), pytest, Docker build. Tests are DB-free by design at this stage; add a Postgres service container for integration tests next.
- **Image**: one multi-stage uv image, non-root, role chosen by command.

## Deliberate MVP cuts (and where they'd go)

| Cut | Where it lands later |
|---|---|
| Source-document-level ACL mirroring (e.g. per-SharePoint-item permissions) | connector emits principal lists in `RawDocument.metadata`; retrieval filter extends to chunk-level |
| S3/Azure blob store | implement `rag_core.blobstore.BlobStore` |
| Embedding cache / batch queue | `rag_core.pipeline.indexer.get_embedder` seam |
| Reranker plugin stage | new entry-point group `rag.rerankers` consumed by retrieval service |
| Token-exact chunk budgets (tiktoken) | swap `estimate_tokens` in the default chunker, or ship a custom chunker plugin |
| Graph API group-overage resolution (>200 groups) | `rag_api.auth.decode_token` |
