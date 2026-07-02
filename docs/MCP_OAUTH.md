# MCP OAuth / SSO for agents (design note)

Status: **exploration, not built.** This captures how interactive SSO ("add the
MCP server to an agent, log in via a browser") would work for our `/mcp`
endpoint, what we already have, and what's left to wire. No code change has been
made for this.

## Goal

Today the MCP server (`fastapi_mcp` at `/mcp`, see `rag_api/main.py`) authenticates
the same way as REST: the caller sends an `Authorization` header carrying either
an Entra JWT or a `ragk_…` API key, validated by `get_principal` →
`decode_token` / `principal_from_api_key` in `rag_api/auth.py`.

That works for headless/service agents (give them an API key). It does **not**
give the "I added this MCP to my agent and a browser popped up to log in" flow,
where each tool call carries the *end user's* identity. That flow is OAuth 2.1
authorization-code + PKCE, and it's what this note is about.

## How the flow works (OAuth 2.1, lazily triggered by a 401)

When a user adds our `/mcp` URL to an MCP client (Claude custom connector,
Cursor, etc.):

1. **Agent calls a tool → `401`** with
   `WWW-Authenticate: Bearer resource_metadata="https://<host>/.well-known/oauth-protected-resource"`.
   The 401 is the trigger; nothing happens until a call is unauthorized.
2. **Agent fetches Protected Resource Metadata** (RFC 9728) → learns which
   authorization server to use (our Entra tenant) and the resource identifier.
3. **Agent fetches Authorization Server Metadata** → learns the `authorize`,
   `token`, and `register` endpoints.
4. **Agent registers as a client** (Dynamic Client Registration, RFC 7591) — or
   uses a pre-configured `client_id`. ← the one friction point (see below).
5. **Agent opens a browser → user logs in via Entra SSO**, PKCE protects the
   exchange. No credentials touch the agent or our server.
6. **Entra redirects back with a code → agent exchanges it for an access token**,
   audience-bound to our API (RFC 8707 resource indicator) so it can't be
   replayed at another app.
7. **Agent retries with `Authorization: Bearer <token>`.** Our existing
   `get_principal` → `decode_token` validates the JWT. The agent then silently
   reuses/refreshes the token.

Net: the agent never sees credentials; our server never sees credentials — it
just validates Entra-issued JWTs, which it already does.

## What we already have (≈60% done)

- **Resource-server half — done.** `decode_token` validates Entra JWTs and
  `get_principal` accepts `Bearer` (`rag_api/auth.py`).
- **The 401 trigger — done.** `AuthConfig(dependencies=[Depends(get_principal)])`
  in `rag_api/main.py` already raises 401 on unauthenticated MCP calls.
- **Per-user RBAC — done and the main payoff.** Once each call carries the real
  user's token, `_readable_collections` (in `rag_api/routers/search.py`) and the
  per-collection roles apply per user automatically — strictly better than a
  shared API key for human-in-the-loop agents.

## What's missing

The **discovery layer**: the `.well-known` metadata endpoints and the
`WWW-Authenticate` pointer that tell a generic agent which IdP to use. Right now
our 401 is a dead end — the agent doesn't know where to send the user.

`fastapi_mcp` 0.4.0 has the machinery. `AuthConfig` exposes:

```
issuer, authorize_url, oauth_metadata_url, audience, default_scope,
client_id, client_secret, custom_oauth_metadata,
setup_proxies, setup_fake_dynamic_registration, metadata_path
```

## The real obstacle: Entra has no open Dynamic Client Registration

The MCP spec wants step 4 to be automatic (RFC 7591). **Entra ID does not expose
open DCR** — we can't let arbitrary clients self-register in the tenant. Two
supported workarounds:

- **(a) Pre-register one app in Entra** (with the agents' redirect URIs, e.g.
  Claude's callback) and hand its `client_id` to `AuthConfig`. Works when the
  client UI lets you paste a `client_id` (most custom-connector UIs do).
- **(b) Let `fastapi_mcp` fake DCR:** `setup_proxies=True` +
  `setup_fake_dynamic_registration=True`. The library stands up proxy
  `.well-known` + a fake `register` endpoint in front of Entra that hands every
  "registering" client our one pre-registered `client_id`. This is exactly the
  bridge for IdPs that lack open DCR. Still requires registering one app in Entra
  and passing `client_id`/`client_secret`.

## Decisions to make before wiring

- **Audience binding.** Set `audience` to our API's resource id so a token minted
  for another Entra app can't be replayed here. **Check first whether
  `decode_token` actually verifies `aud` today** — if not, harden it alongside
  this work.
- **Public client + PKCE.** The Entra app registration must allow auth-code +
  PKCE and list the agents' redirect URIs.
- **Spec version.** `fastapi_mcp` 0.4.0 implements the MCP `2025-03-26` auth
  revision (server proxies the auth server). The newer `2025-06-18` revision
  cleanly splits resource vs. auth server via PRM. Most clients are
  backward-compatible, but if a specific client insists on the PRM-only flow we
  may need to serve `.well-known/oauth-protected-resource` ourselves rather than
  relying solely on the proxy.

## API keys are not going away

OAuth/SSO is for **human-in-the-loop** agents (identity follows the person,
revocation = disabling the account). **API keys remain the right tool for
headless/service agents** with no human to log in. The two are complementary.

## Rough work breakdown when we pick this up

1. Register one app in Entra (auth-code + PKCE, redirect URIs for target clients,
   API scope/resource id).
2. Extend `AuthConfig` in `rag_api/main.py` with `issuer` / `authorize_url` /
   `audience` / `client_id` / `client_secret`, plus `setup_proxies` +
   `setup_fake_dynamic_registration`.
3. Confirm/serve the `.well-known` metadata and the `WWW-Authenticate` resource
   pointer on 401.
4. Harden `decode_token` to verify `aud` if it doesn't already.
5. Test the end-to-end browser login from at least one real MCP client.
