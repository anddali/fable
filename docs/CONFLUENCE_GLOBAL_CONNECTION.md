# Change plan: global Confluence connection + paste-a-link targets

Status: **implemented** (branch `feat/confluence-global-connection`). Turns "add
a Confluence source" from *type base_url + creds + hand-write a nested `targets`
JSON* into *paste a page/space URL, pick the collection*. Credentials become a
single operator-configured service account (an env var), not a per-source field.

Delivered: `ConfluenceSettings`/`ConfluenceConnection` + `Settings.confluence`
([config.py](../packages/rag-core/src/rag_core/config.py)); `ready` /
`parse_target_url` / `supports_url_targets` on `ConnectorPlugin`
([interfaces.py](../packages/rag-core/src/rag_core/interfaces.py)); connector
refactor + URL parser + legacy fallback
([connector.py](../plugins/rag-connector-confluence/src/rag_connector_confluence/connector.py));
`POST …/sources/test`, link-aware create, readiness gating, error shaping
([sources.py](../packages/rag-api/src/rag_api/routers/sources.py)); `/plugins`
readiness annotation; the add-source UI link form
([app.js](../packages/rag-api/src/rag_api/static/app.js)); compose env; tests
([test_confluence_connection.py](../tests/test_confluence_connection.py)). Named
connections are config-driven (`RAG_CONFLUENCE__CONNECTIONS`); a DB-backed
connections table + RBAC remains the deferred extension.

Built with a **door-open** strategy: exactly one connection (`"default"`) exists
today, but the data model and resolver seam already carry a connection *name*, so
named/multiple connections can be added later with no migration of existing
sources.

## Goals

- End users adding a Confluence source provide only: a **link** and a **name**.
  No `base_url`, no `user_email`, no token.
- Credentials live once, as a service-account token in an env var, consistent
  with the platform's "store env-var *names*, never secrets" stance
  ([models.py:90](../packages/rag-core/src/rag_core/db/models.py:90)).
- Fail fast: a misconfigured connection or bad link is reported at
  create/test time, not silently at the next sync.
- Keep the single-identity trade-off explicit (see Security) and leave room for
  per-collection scoped accounts later.

## Design

### Where connection config lives

Today `base_url` / `user_email` / `api_token_env` are per-source in
`ConfluenceConfig` ([connector.py:28](../plugins/rag-connector-confluence/src/rag_connector_confluence/connector.py:28)).
They move to **global settings**; the source keeps only *what to pull*.

- **Connection** (global, operator-owned): `base_url`, `user_email`,
  `api_token_env`.
- **Source config** (per-source): `targets`, `page_limit`, and an optional
  `connection: str = "default"` selector — the door-open hook.

The connector reads the connection from `ctx.settings` at sync time (the seam
already exists at [sync.py:86-89](../packages/rag-worker/src/rag_worker/sync.py:86)).

### Door-open seam

- Source config carries `connection` (defaults to `"default"`). Stored now, used
  by a resolver that today only knows `"default"`.
- A single resolver function is the *one* place connection lookup happens. Today:
  `"default"` → the configured global connection; anything else → error. Later:
  look up a named connection (settings map, then a `connections` table) without
  touching sources or the connector body.

## Changes by component

### 1. `rag_core/config.py` — settings

Add a nested block alongside `auth` / `embedding` / `chat`
([config.py:109-111](../packages/rag-core/src/rag_core/config.py:109)):

```python
class ConfluenceConnection(BaseModel):
    base_url: str = ""          # https://org.atlassian.net/wiki
    user_email: str = ""        # service-account email
    api_token_env: str = "CONFLUENCE_API_TOKEN"

    @property
    def configured(self) -> bool:
        return bool(self.base_url and self.user_email)

class ConfluenceSettings(BaseModel):
    """Default (and, for now, only) Confluence connection."""
    base_url: str = ""
    user_email: str = ""
    api_token_env: str = "CONFLUENCE_API_TOKEN"

# on Settings:
confluence: ConfluenceSettings = Field(default_factory=ConfluenceSettings)

def confluence_connection(self, name: str = "default") -> ConfluenceConnection:
    """Door-open resolver — the single place name→connection is resolved."""
    if name == "default":
        c = self.confluence
        return ConfluenceConnection(
            base_url=c.base_url, user_email=c.user_email,
            api_token_env=c.api_token_env,
        )
    raise ValueError(f"Unknown Confluence connection {name!r}")
```

Env vars (nested delimiter `__`): `RAG_CONFLUENCE__BASE_URL`,
`RAG_CONFLUENCE__USER_EMAIL`, `RAG_CONFLUENCE__API_TOKEN_ENV`.

### 2. `interfaces.py` — optional link-parsing hook (generalizes beyond Confluence)

Add to `ConnectorPlugin` a classmethod other connectors can implement later
(SharePoint/Git URLs):

```python
@classmethod
def parse_target_url(cls, url: str) -> dict | None:
    """Best-effort: URL -> a config fragment (e.g. a target). None if unsupported."""
    return None
```

### 3. Confluence connector

- **`ConfluenceConfig`**: drop `base_url` / `user_email` / `api_token_env`; keep
  `targets`, `page_limit`; add `connection: str = "default"`.
- **`iter_documents`**: resolve the connection via
  `ctx.settings.confluence_connection(cfg.connection)`; take `base_url` /
  `user_email` from it, token from `os.environ[conn.api_token_env]` (unchanged
  error if missing).
- **`parse_target_url`**: parse canonical Cloud URLs →
  `{"kind", "id"}` (space `…/spaces/KEY`, page `…/pages/{id}`, folder
  `…/folder/{id}`). Return the space key / numeric id. `page` vs `tree` is caller
  intent (the "include children" toggle), defaulting to `page`.
- **Back-compat**: if a legacy source still has `base_url` in its `config`, prefer
  it over the connection (so existing sources keep working with no migration).
  Emit a one-time deprecation log.

Short `/x/…` links: resolved server-side (one redirect) in the test endpoint,
not in the pure parser.

### 4. API — `sources.py` + a test/resolve endpoint

- **`POST …/sources:test`** (new): instantiate the connector, resolve the
  connection, check the token env is set, resolve the link (following redirects)
  → canonical target, fetch **one** page. Return `{ok, target, sample_title}` or
  a structured error. This is the fail-fast + link-resolve path.
- **`create_source`**: for `connector == "confluence"`, accept a `link` (+
  `include_children`) and derive `targets` via `parse_target_url`, or accept
  `targets` directly. Reject when the selected connection isn't `configured`
  (422, clear message) and when the link host ≠ connection `base_url` host.
- Shape validation errors as field-path + message instead of raw
  `exc.errors()` ([sources.py:28](../packages/rag-api/src/rag_api/routers/sources.py:28)).

### 5. UI — `app.js` add-source modal

- Advertise per-connector readiness (is the connection configured?) via the
  existing `/ui-config` fetch ([app.js:39](../packages/rag-api/src/rag_api/static/app.js:39))
  or `/plugins`.
- When `connector == confluence` and ready: render **name + link field +
  "include child pages" toggle + a Test button**; hide the raw-JSON textarea.
  When not ready: show "Confluence isn't configured on this server" instead of a
  broken form.
- Keep the raw-JSON textarea as the generic fallback for connectors without a
  bespoke form (unchanged path for git/slack/etc.).

### 6. `docker-compose.yml` — plumbing (the current footgun)

Add to the `&app-env` anchor ([docker-compose.yml:22](../docker-compose.yml:22))
so the worker actually has the token and the connection:

```yaml
CONFLUENCE_API_TOKEN: ${CONFLUENCE_API_TOKEN:-}
RAG_CONFLUENCE__BASE_URL: ${RAG_CONFLUENCE__BASE_URL:-}
RAG_CONFLUENCE__USER_EMAIL: ${RAG_CONFLUENCE__USER_EMAIL:-}
RAG_CONFLUENCE__API_TOKEN_ENV: ${RAG_CONFLUENCE__API_TOKEN_ENV:-CONFLUENCE_API_TOKEN}
```

## Security / governance trade-off (accept consciously)

One service account = **one permission boundary**. Anything the account can read
in Confluence becomes indexable by anyone who can add a source to a collection
they edit, and then searchable by that collection's viewers. For a central
internal KB this is the desired simplicity. It cannot express "collection X may
index only HR's restricted space via a scoped account" — that's what named
connections (door-open) are for later.

## Back-compat & migration

- No schema migration. `Source.config` is JSONB; the new optional `connection`
  field defaults to `"default"`, and legacy connection fields are honored if
  present. Existing Confluence sources keep working untouched.
- Once the global connection is configured, legacy sources can be simplified by
  dropping their inline creds (optional cleanup, not required).

## Test plan

- **config**: `RAG_CONFLUENCE__*` parses; `confluence_connection("default")` vs
  unknown-name error.
- **parse_target_url**: space / page / folder URLs → correct `{kind,id}`; host
  mismatch rejected; non-Confluence URL → None.
- **connector**: reads connection from `ctx.settings` (mock httpx); missing token
  env raises; legacy inline config still works (back-compat).
- **API**: create-with-link happy path; connection-not-configured → 422;
  `:test` endpoint happy + token-missing + bad-link cases.
- **UI**: (manual/preview) confluence shows link form; unconfigured shows notice.

## Sequencing

1. **Plumbing + settings + connector refactor** (compose env, `ConfluenceSettings`,
   connection resolver, connector reads `ctx.settings`, back-compat fallback).
   Ships the service-account model; sources still take `targets`.
2. **Link parsing + `:test` endpoint** (`parse_target_url`, resolve/test route,
   `create_source` accepts `link`). Ships paste-a-link + fail-fast.
3. **UI form** (link field + toggle + Test button; readiness gating).
4. *(Later, when needed)* named connections: extend the resolver to a settings
   map, then a `connections` table + RBAC. Sources already carry `connection`.

## Out of scope

- Storing raw tokens in the DB / a pasted-in-UI secret (would require an
  encrypted secrets subsystem — a separate, larger decision).
- Target-discovery pickers (list spaces/pages in the UI).
- Applying the `parse_target_url` hook to other connectors (interface is added;
  implementations come with each connector).
