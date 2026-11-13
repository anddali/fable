# Flow 3: Admin → Integrations

Spec: §5.1, S-1 to S-10, S-16, §5.3 CF-1 to CF-11, §20.3. Mockup: [`../admin-integrations.html`](../admin-integrations.html).

## Wireframe

```
Integrations                                                           [+ Add integration]
Connections to Confluence, SharePoint, Slack and Git. Sources use these; they never hold credentials.
┌ ⚠ 2 connections need attention. "Confluence – HR" lacks a scope…  [Show only these] ┐
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ [⌕ Filter]  (+ Kind) (Status: Any ▾)                                     6 connections │
│ Name ↓                  │ Kind       │ Usable by        │ Sources │ Credential   │ Status│
│ Confluence – general    │ Confluence │ All users        │ 142     │ OAuth · none │ ✓     │
│ Confluence – HR         │ Confluence │ Group HR-Editors │ 6       │ Token · 2027 │ ⊗ Needs attention │
│ GitHub – docs bot       │ Git        │ All users        │ 23      │ expires 9 d  │ ⏱ Expiring │
│ SharePoint – HR tenant  │ SharePoint │ Group HR-Editors │ 4       │ Secret · 2027│ ✓     │
│ Slack – eng workspace   │ Slack      │ Admins only      │ 3       │ Bot token    │ ✓     │
│ Confluence (imported)   │ Confluence │ All users        │ 11      │ none         │ 🔒 Credentials required │
└────────────────────────────────────────────────────────────────────────────────────────┘
                      ┌─ drawer: Confluence – HR ──────────────────────────────── ✕ ─┐
                      │ ⊗ 4 of 5 checks passed · tested 1 min ago                     │
                      │   ✓ Token accepted  ✓ Space lookup · 3 spaces  ✓ Pages ✓ Body │
                      │   ✗ Attachment download · 403. Probably lacks                 │
                      │     read:attachment:confluence. Add the scope, then test.     │
                      │ Name · Site URL · Cloud ID (auto, editable, "not the org ID") │
                      │ Authentication (OAuth client credentials | Scoped API token)  │
                      │ API token: Set · ends …Q7mz · updated 3 days ago  [Replace]   │
                      │ Token expires on [2027-03-01]                                 │
                      │ Usable by [HR-Editors ✕][Add Entra group…]                    │
                      │ [Delete…]                               [Test again] [Save]   │
                      └───────────────────────────────────────────────────────────────┘
```

## Behaviour

* **Add integration** → kind picker (Confluence, SharePoint, Slack, Git), then the schema-generated form
  in the drawer. It is the same form as setup step 3.
* **Confluence specifics** (CF-2 to CF-5):
  * The site URL triggers a cloud-ID lookup on blur. The Cloud ID field is pre-filled, editable, and
    carries the help text "This is not the organization ID".
  * Two auth choice cards: *OAuth client credentials* (recommended, client ID + secret) and *Scoped API
    token* (token + required expiry date, 1–365 days).
  * There is no email field and no personal token option. A callout explains that a service account is
    required and links to Atlassian's docs.
* **Test** (S-5, CF-6) lists every endpoint the connector uses with ✓/✗. A failure shows the HTTP code,
  the likely missing scope and what to do. The result is stored on the row: the "Status" column plus
  "tested n min ago".
* **Status values** shown as pills: *Healthy* (last test ok), *Needs attention* (last test failed, or
  credential expired), *Expiring* (≤ 30 days, S-16), *Credentials required* (imported without a secret,
  CF-10), *Disabled*.
* **Usable by** (S-7): a picker for *Admins only*, *All signed-in users*, or Entra groups as chips (with
  group typeahead). The help text explains the consequence.
* **Sources column** links to the usage view (`GET /admin/connections/{id}/usage`): which collections and
  sources use the connection.
* **Delete**: if it is in use, a 409 dialog lists the dependents and offers no delete. If unused, the
  admin types the connection name to confirm (UX-3).
* **Disable** (row menu) marks dependent sources as failing fast with "Connection disabled by an admin".
* Changes are audited (S-10). The drawer footer links "View history", which is the audit log filtered to
  this entity.

## Attention surfacing

* A banner at the top of this page counts connections that need attention, with a "Show only these" filter.
* Admin → Overview shows the same count. The sidebar "Integrations" item carries a small warning count.
* Collection pages that use a failing or expiring connection show a pill next to the source (see
  [flow 6](06-collection-detail.md)).
