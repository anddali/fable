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
| Documents | Paged table (UX-11): title, source, status (indexed/pending/failed with error tooltip), chunks, updated, indexed; filters for status, source and type; `q` on title | Bulk retry failed, Reindex; the document drawer shows metadata, error, and a paged chunk viewer (`from_ordinal`), with each chunk's heading path and context line shown separately |
| Syncs | `sync_runs` history per source: started, duration, listed/added/changed/removed/failed, outcome | Open run details, Retry |
| Indexing settings | See [flow 7](07-indexing-settings.md) | Save and rebuild |
| Access | Owner, editors and readers (users and Entra groups) with typeahead; "Visible to everyone" toggle | Add, change role, remove; audited |
| Evaluation | See [flow 10](10-evaluation.md) | Run, compare |
| Feedback | Paged list of chat feedback for this collection (E-6): rating, comment, question, answer excerpt, retrieved chunks, date; filter 👍/👎/has comment | "Add to evaluation set" turns a 👎 into a golden item in one click |
