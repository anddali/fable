# Flow 4: Add a Confluence source by pasting a link

Spec: UX-13, S-7, S-11, S-14, CF-4, CF-7, CF-8; `parse_target_url` design in
[`../../CONFLUENCE_GLOBAL_CONNECTION.md`](../../CONFLUENCE_GLOBAL_CONNECTION.md). Mockup: [`../source-confluence.html`](../source-confluence.html).

## Goal

A collection editor with no admin rights and no credentials adds a Confluence page tree or space by
pasting the URL from their browser. They never see base URLs, tokens or a JSON `targets` blob.

## Entry points

* Collection → Sources → **Add source** (drawer)
* Collection → Overview → Sources card → **Add source**
* Setup wizard step 5 (inline, same component)
* Pasting a URL into the empty Sources tab, which pre-fills the drawer

## Wireframe

```
┌─ Add a source · to Engineering handbook ──────────────────────────────────────── ✕ ─┐
│ ( 🔗 Paste a link | ⇪ Upload files | Advanced… )                                     │
│ Link to a Confluence page or space                                                  │
│ [🔗 https://acme.atlassian.net/wiki/spaces/PLAT/pages/884211/Platform+runbooks   ]   │
│ Space overview, page, or short /x/… links all work. SharePoint, Slack, Git too.     │
│ ┌─────────────────────────────────────────────────────────────────────────────────┐ │
│ │ ▤ Platform runbooks                                                          ↗  │ │
│ │   Page in space PLAT · Platform · 37 child pages · edited 3 days ago            │ │
│ │ ( ● This page and everything below it   38 pages now; new children picked up )  │ │
│ │ ( ○ Only this page                       1 page )                               │ │
│ │ ( ○ The whole PLAT space                 612 pages )                            │ │
│ └─────────────────────────────────────────────────────────────────────────────────┘ │
│ Source name [Platform runbooks]        Connection [Confluence – general ▾]          │
│                                        Matched by site. Only ones you may use.      │
│ ▸ Options (page limit, sync schedule)                                               │
│ ✓ Ready. Fetched "Platform runbooks" as the service account (0.6 s).                │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ [Cancel]                                                  [Add source and sync]     │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

## Behaviour

1. **Paste or type** a link. Resolution starts on paste, or 500 ms after typing stops, by calling
   `POST /collections/{id}/sources/test` with `{url}`. A "Checking the link…" row shows meanwhile.
2. The server picks the connector by URL (`parse_target_url`), resolves short `/x/…` links, and picks the
   **connection**: among those the caller may use (S-7) whose `site_url` matches the link's host. If
   several match, the first by name is pre-selected and the select lists all of them. Links to other
   kinds (SharePoint, Slack, Git) switch the preview card to that kind.
3. The **preview card** shows what the link points to (page or space), its title, its space, its child or
   page count and when it was last edited, all fetched as the service account. This proves access before
   saving.
4. **Scope choice** for a page link: *page and everything below it* (default, the tree target), *only
   this page*, or *the whole space*. A space link shows the space only. Counts are approximate ("~612") if
   the API doesn't return totals cheaply.
5. **Name** defaults to the page or space title and stays editable.
6. **Add source and sync** creates the source (`POST /collections/{id}/sources` with
   `{connection_id, targets:[…]}`), enqueues a sync in the same transaction, closes the drawer, and shows
   the sync progress on the Sources tab plus a toast when it finishes.

## Error and edge states

| Case | Message (title. body) | Action |
|---|---|---|
| Not a recognised URL | "That doesn't look like a Confluence page or space link. Copy the address from your browser while viewing the page." | Input stays focused, `aria-invalid` |
| Service account can't see it (403/404) | "The service account can't see this page. Ask a Confluence admin to grant 'Confluence – general' access to space HR, or pick another connection." | Connection select stays enabled |
| No usable connection for the host | "No connection you can use covers globex.atlassian.net. Ask an admin to add one." | "Copy request" copies a prefilled message for the admin |
| Connection needs attention | "Confluence – general needs attention (credentials expired). Syncs will fail until an admin fixes it." | Add is allowed, with a warning |
| Already a source in this collection | "This page is already included by 'ENG space'." | Link to the existing source |
| Rate-limited (429) | "Confluence is busy. Retrying in 12 s…" | Automatic retry |

## Accessibility

* The preview card is `aria-live="polite"`. The scope choice is a radiogroup with a `legend`.
* All error messages are associated with the input via `aria-describedby`.
