# Flow 6: Collection detail tabs

Spec: §20.3, UX-7, UX-9, UX-11, PS-3/PS-4, E-8, E-6. Mockup: [`../collection-detail.html`](../collection-detail.html)
(Overview). Sibling tabs: [`../source-confluence.html`](../source-confluence.html) (Sources),
[`../indexing-settings.html`](../indexing-settings.html), [`../evaluation.html`](../evaluation.html).

## Header (all tabs)

```
Engineering handbook ★                         [⌕ Search this collection] [⟳ Sync now] [⋯]
Runbooks, ADRs… · Owner Dana Kowalski · Visible to everyone
[Overview] [Sources 2] [Documents 1,486] [Syncs] [Indexing settings •] [Access] [Evaluation] [Feedback 3 new]
```

* The tabs are route segments (`/ui/collections/{id}/documents`), so each one can be linked.
* Tabs a role can't use are hidden. Readers see Overview, Documents and Sources (read-only) only.
  Indexing settings, Access, Evaluation and Feedback require editor or owner (owner for Access).
* The "⋯" menu has Rename, Transfer ownership (owner/admin), Export eval sets and Delete… (type-to-confirm).

## Overview tab

```
┌ Documents ┐┌ Chunks ┐┌ Searches · 7d ┐┌ Recall@5 · latest eval ┐
│ 1,486     ││ 22,904 ││ 2,318         ││ 0.86 ▲ 0.06            │
│ 1,471 idx · 3 failed · 12 pending ...                           │
┌ ⟳ Re-chunking after settings change ─ started 4 min ago by Dana ┐  ┌ Indexing settings  Edit ┐
│ 812 of 1,486 documents          about 3 min left · no fetch     │  │ Chunks 1,000 · min 200  │
│ ██████████░░░░░░░░                                              │  │ Context line On · haiku │
│ Search keeps serving previous chunks… View change · Run eval    │  │ Vision · Language · Emb │
└─────────────────────────────────────────────────────────────────┘  ├ Latest evaluation  Open ┤
┌ Sources                                         [+ Add source] ┐  │ R@1/5/10 · MRR · config │
│ ENG space · Confluence – general · 1,240 · ✓ 18 min · +4 ~11 −0│  ├ Recent feedback    All  ┤
│ ADR repo · GitHub – docs bot ⏱ expires 9 d · 246 · ⚠ 3 failed  │  │ 👎 "Didn't find…"       │
└─────────────────────────────────────────────────────────────────┘  └─────────────────────────┘
```

* Rebuilds and syncs in progress show as progress cards at the top (`GET /collections/{id}/rebuilds`,
  `…/syncs`). They update live through polling or SSE, with an `aria-live` region and a toast on finish.
* The "Latest evaluation" card shows the metrics **next to the settings that produced them** (E-8).
* A connection problem on a source shows inline (pill on the connection name), linking to the admin page
  for admins and showing "Ask an admin" for others.

## Other tabs (layout notes; same table component)

| Tab | Content | Key actions |
|---|---|---|
| Sources | Table: source, kind and target, connection (with health pill), documents, last sync (+added ~changed −removed), next run, enabled toggle | Add source ([flow 4](04-confluence-paste-a-link.md)), Edit, Sync, Force resync…, Disable, Delete |
| Documents | Paged table (UX-11): title, source, status (indexed / pending / failed with error tooltip / **re-upload required**), chunks, updated, indexed; filters for status (incl. *Re-upload required*), source and type; `q` on title | Bulk retry failed, Reindex, **Re-upload…** (see below); the document drawer shows metadata, error, and a paged chunk viewer (`from_ordinal`), with each chunk's heading path and context line shown separately |
| Syncs | `sync_runs` history per source: started, duration, listed/added/changed/removed/failed, outcome | Open run details, Retry |
| Indexing settings | See [flow 7](07-indexing-settings.md) | Save and rebuild |
| Access | Owner, editors and readers (users and Entra groups) with typeahead; "Visible to everyone" toggle | Add, change role, remove; audited |
| Evaluation | See [flow 10](10-evaluation.md) | Run, compare |
| Feedback | Paged list of chat feedback for this collection (E-6): rating, comment, question, answer excerpt, retrieved chunks, date; filter 👍/👎/has comment | "Add to evaluation set" turns a 👎 into a golden item in one click |

## Documents tab: re-upload required

Upload sources can't be re-fetched (ST-5, FAB-140). After a change that needs a **re-parse** (parser,
transformers or vision settings, see [flow 7](07-indexing-settings.md)), Fable re-parses connector
documents from their sources. Upload documents get the status **Re-upload required** instead of being left
silently stale. Chunk-size and context-line changes never cause this; they rebuild from stored chunks.

```
[All 1,486] [Indexed] [Pending] [Failed 3] [Re-upload required 212]      ← status filter (saved views)
┌ ⚠ 212 uploaded documents were indexed with the previous parser settings. They still appear in search
│   with their old chunks. Upload them again to apply the new settings.        [Re-upload files…] ┐
│ ☐ Title                 │ Source         │ Status                 │ Chunks │ Uploaded   │   ⋯ │
│ ☐ Q3-architecture.pdf   │ Design uploads │ ⇪ Re-upload required   │ 41     │ 2026-09-02 │   ⋯ │
│ ☐ vendor-contract.docx  │ Design uploads │ ⇪ Re-upload required   │ 18     │ 2026-08-11 │   ⋯ │
```

* **Status pill**: warning tone, upload icon, label "Re-upload required". The tooltip names the change that
  caused it ("Parser changed by Dana Kowalski on 12 Nov"). It is a document status, so it also appears in
  the document drawer, in the collection Overview counts ("212 need re-upload") and in the "Needs
  attention" saved view of the collections list ([flow 5](05-collections-list.md)).
* **Filter**: `status=reupload_required` is a value of the Documents status filter, with its own count.
  The callout above the table appears whenever the count is non-zero, on any filter.
* **Search behaviour**: these documents keep serving their previous chunks (R-2, PS-4) until replaced.
* **Re-upload action**: "Re-upload files…" (callout, bulk action on selected rows, and the row menu for
  one document) opens the upload drawer for the document's upload source, listing the expected file
  names. Uploaded files are matched by file name to the existing documents and replace them in place,
  which re-parses with the current settings and clears the status. Files that don't match are offered as
  new documents. The drawer shows "n of 212 matched".
* **Dismiss instead**: owners and editors can bulk **Delete** documents they no longer need. There is no
  "keep stale and clear the flag" action, because the point of the status is to avoid silently stale
  chunks.
* The impact preview ([flow 7](07-indexing-settings.md)) states the number of upload documents that will
  get this status before the change is saved.
