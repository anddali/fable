# Flow 1: First-run setup wizard

Spec: UX-13, UX-14, S-15, §5.4 MP-1 to MP-4, §5.3, C-4. Mockup: [`../setup.html`](../setup.html) (`?step=1` to `?step=6`).
Screens: `screens/setup-*-{light,dark}.png`.

## Goal

A fresh install takes a first admin from "empty database" to "a search that returns results" in about
10 minutes, without reading docs. Each step proves itself with a **test** before the admin moves on.

## Entry and exit

* **Entry:** an admin signs in and `/ui-config` reports `setup.complete = false`, meaning no embedding role
  is configured. The SPA routes to `/ui/setup`. Non-admins see a "Fable is being set up by your admin"
  page with the admin contact instead.
* If C-4 imported legacy env values, the wizard pre-fills steps 1–3 and marks them "Imported, please test".
* **Resume:** progress is derived from server state (providers, roles, connections, collections), not
  from client storage. Closing the tab and returning resumes at the first incomplete step.
* **Exit:** after step 6, or **"Finish later"** (available once step 2 is done; the workspace is usable).
  Admin → Overview offers "Re-run setup".

## Steps

| # | Step | Required | Test that must pass | API |
|---|---|---|---|---|
| 1 | Model provider | yes | `/models` or a 1-token call, showing the model count by capability and latency | `POST /admin/connections/test`, `POST /admin/connections` |
| 2 | Model roles | Embedding yes; Chat, Context line, Vision optional | Embedding embeds a sample sentence (shows dimension and latency); Chat answers a sample question | `PUT /admin/settings/embedding` (+ chat/vision/context) |
| 3 | Integration | no ("Skip" if uploads only) | Per-endpoint check (Confluence CF-6, Graph token, Slack `auth.test`, `git ls-remote`) | `POST /admin/connections/test`, `POST /admin/connections` |
| 4 | First collection | yes | none (validated form) | `POST /collections` |
| 5 | Add a source | no ("Skip, I'll upload files") | Paste-a-link resolve and fetch of one item (see [flow 4](04-confluence-paste-a-link.md)) | `POST /collections/{id}/sources/test`, `POST …/sources` |
| 6 | First search | no | Live sync progress, then a real search | `GET /collections/{id}/syncs`, `POST /search` |

## Wireframe (shared layout)

```
┌───────────────────────────┬──────────────────────────────────────────────────────────────┐
│ [F] Fable                 │  Step 3 of 6                                                  │
│                           │  Connect where your documents live                            │
│ Set up Fable              │  One integration is enough to start. Only uploading? Skip ›   │
│ About 10 minutes…         │                                                               │
│                           │  ( Confluence )( SharePoint )( Slack )( Git )   ← radio cards  │
│ (✓) Model provider        │  ┌──────────────────────────────────────────────────────────┐ │
│  │  OpenAI · tested       │  │ ⓘ Needs an Atlassian service account, not a personal… ↗  │ │
│ (✓) Model roles           │  │ Name [Confluence – service account] Site URL [https://…] │ │
│  │  Embedding required    │  │                                     ✓ Cloud ID found     │ │
│ (3) Integration    ◀ here │  │ Authentication ( ● OAuth client creds )( ○ Scoped token )│ │
│  │                        │  │ Client ID [……]            Client secret [••••••]         │ │
│ (4) First collection      │  │ Who can use [All signed-in users ▾]                      │ │
│ (5) Add a source          │  │ ┌ ✓ All 5 checks passed · 41 spaces visible ───────────┐ │ │
│ (6) First search          │  │ │ ✓ Token  ✓ Spaces  ✓ Pages  ✓ Body  ✓ Attachments    │ │ │
│                           │  │ └──────────────────────────────────────────────────────┘ │ │
│ 🔒 Credentials are        │  └──────────────────────────────────────────────────────────┘ │
│    encrypted…             │  ‹ Back                                   [Skip] [Save and continue ›] │
└───────────────────────────┴──────────────────────────────────────────────────────────────┘
```

Step 2 (roles) is an accordion. Embedding starts expanded and marked *Required*. The other roles are rows
with "Set up" buttons. Each configured role shows a one-line test summary.

Step 6 shows the sync progress card (`aria-live`) above a real search box pre-filled with a suggested
query, built from the most frequent heading in the first indexed pages.

## Behaviour

* **Continue is disabled until the step's test passes**, with the hint "Test passes before you can
  continue". A changed field invalidates the last test result, which turns grey with "Settings changed,
  test again".
* Test runs automatically 600 ms after the last required field is filled. A manual "Test again" button
  stays visible.
* Provider cards: picking one swaps the form fields, which are generated from that kind's JSON Schema.
* Model selects list discovered models filtered by capability (MP-3). "Enter a model name manually" is
  always available; an unknown capability shows a warning but is allowed.
* The Embedding model choice shows the dimension and a note: "Changing it later re-embeds every chunk
  (blue/green, no downtime)".
* The Confluence site URL triggers the cloud-ID lookup on blur. Success shows "✓ Cloud ID found" with an
  Edit link. Failure shows "Couldn't find a Confluence Cloud site at this address" with the field editable.
* Step 4's "Who can search it" defaults to *Everyone signed in* in the wizard, because the first
  collection is a shared demo. The regular "New collection" form defaults to *Only me*.
* Step 4 shows the platform indexing defaults read-only, with "Change later in Indexing settings".

## States

| State | Treatment |
|---|---|
| Test running | Callout with a spinner, "Testing… (OAuth token, spaces, pages…)". Continue disabled |
| Test failed | `.callout-danger` listing each check, the failing one with code and likely cause, e.g. "403 on attachment download. The token probably lacks `read:attachment:confluence`" plus a docs link |
| Provider unreachable | "Couldn't reach http://ollama:11434 (connection refused). Is the container on the same network?" |
| Imported (C-4) | Pill "Imported from environment", with fields pre-filled and secrets missing ("Enter a service-account credential") |
| Non-admin | Full-page "Fable isn't set up yet. Your admin (Dana Kowalski) has been notified." |

## Accessibility

* The stepper is an `<ol>` with `aria-current="step"`. Completed steps are links, future steps are plain text.
* Each step is a `<section aria-labelledby>`. Focus moves to the step heading on navigation.
* Test results are announced via `role="status"`.
