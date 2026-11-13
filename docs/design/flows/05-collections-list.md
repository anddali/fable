# Flow 5: Collections list at scale

Spec: UX-8 to UX-12, §20.3. Mockup: [`../collections.html`](../collections.html).

## Goal

Find, triage and act on collections when there are 1,000+ of them, with first-page time under 500 ms (UX-12).

## Wireframe

```
Collections                                                             [+ New collection]
Collections you can read. Admins see every collection under Admin → Collections.
[All 1,284] [Owned by me 14] [Favourites 6] [Needs attention 9]          ← saved views (tabs)
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ [⌕ Search name or description] (Status: Any ▾) (My role: Any ▾) (+ Owner) (+ Connector) [◉ Columns] │
├────────────────────────────────────────────────────────────────────────────────────────┤
│ 2 selected  [⟳ Sync now] [⟳ Retry failed documents] [☆ Add to favourites]  Clear selection │
├──┬──────────────────────────┬──────────────────┬───────┬───────┬──────────┬─────────┬─────┬──────────┬─┤
│☐ │ Name ↓                   │ Status           │ Docs  │ Chunks│ Last sync│ Owner   │ Role│ Visible to│⋯│
│☐ │ Engineering handbook ★   │ ✓ Healthy        │ 1,486 │ 22,904│ 18 min   │ Dana K. │Owner│ Everyone  │⋯│
│☐ │ Platform incidents ★     │ ⟳ Indexing · 63% │ 3,210 │ 41,770│ running  │ Sam O.  │Edit │ Everyone  │⋯│
│☑ │ HR policies              │ ⚠ 3 failed docs  │   412 │  5,117│ 2 h      │ Priya R.│Read │ HR-All    │⋯│
│☑ │ Finance – FY27 planning  │ ⊗ Sync failed    │    77 │  1,203│ 1 d      │ Marta N.│Read │ Finance   │⋯│
│  │ …                                                                                   │
├──┴─────────────────────────────────────────────────────────────────────────────────────┤
│ 1–25 of 1,284                               Rows per page [25 ▾]  [‹ Previous] [Next ›] │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

## Behaviour

* The data comes from `GET /collections?limit=25&cursor=…&sort=name&q=…&status=…&role=…&owner=…&connector=…`.
  The response includes `total` and per-row status aggregates (counts only, no document lists).
* **Status** is derived on the server: *Healthy*, *Indexing n%* (any sync or rebuild running), *n failed
  docs*, *Sync failed* (last sync errored), *Empty* (no sources), *Connection needs attention*.
* **Saved views** are preset filters. "Needs attention" combines failed docs, failed syncs and
  connection problems.
* **Bulk actions** are limited to what every selected row allows (role-aware). Actions the role doesn't
  allow are disabled with a tooltip ("You're a reader in 1 selected collection"). They run as a job, and
  progress shows in a toast.
* **Row menu** (⋯): Open, Search in this collection, Sync now, Favourite, Copy link, Delete… (owner only;
  type-to-confirm).
* **Columns** menu: toggle Chunks, Owner, Visible to, Last sync, Created. The choice persists per user in
  localStorage.
* The header checkbox selects the page. A link "Select all 1,284 matching" appears after it, used only for
  bulk actions that support filter-based selection.
* URL state: `?view=all&sort=name&dir=asc&q=&status=&cursor=…&limit=25`.
* **Favourites** are per user and also feed the collection picker's Favourites group.

## States

| State | Treatment |
|---|---|
| Loading | 10 skeleton rows, toolbar enabled |
| No collections at all | "No collections yet. A collection groups sources that are searched together. [+ New collection]" |
| No matches | "No collections match 'plat' with Status: Sync failed. [Clear filters]" |
| Error | Inline danger callout in the table body with Retry. Toolbar state is kept |

## Performance notes for implementers

* Keyset pagination on `(sort_key, id)`. Never `OFFSET` on large tables.
* `q` uses trigram or `ILIKE` on name and description, with an index.
* Status aggregates come from maintained counters on `collections` (or a materialised view), not a
  `COUNT(*)` over documents per row.
