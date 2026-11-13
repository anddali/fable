# Flow 8: Search with collection picker and filters

Spec: §12.1 R-5/R-6, §12.2 F-1 to F-6, UX-10, CR-3. Mockup: [`../search.html`](../search.html).

## Wireframe

```
[⌕ when should we scale the ledger worker                        ] (Hybrid | Semantic) [Search]
[In  (Engineering handbook ✕) (Payments service ✕) plat▏                                      ]
 ┌──────────────────────────────────────────────┐
 │ ☐ All readable collections            1,284  │      ← popover (picker open)
 │ MATCHES "PLAT" · 4                           │
 │ ☐ Platform incidents     3,210 docs · Everyone│
 │ ☐ Platform runbooks (arch.)   412 · Everyone │
 │ FAVOURITES  ☑ Engineering handbook        ★  │
 │ RECENT      ☑ Payments service  searched 1 h │
 │ ↑↓ move  Space toggle  Esc close             │
 └──────────────────────────────────────────────┘
┌ ⛛ Filters      Clear ┐  10 results · 2 collections · Confluence · last 90 days · 184 ms [Ask in chat]
│ UPDATED              │  ┌──────────────────────────────────────────────────────────────────┐
│ (Any|7 d|90 d|Custom)│  │ Payments service · Scaling runbook                       0.0328 │
│ CONNECTOR            │  │ [Confluence] Payments › Operations › Autoscaling · upd 12 d     │
│ ☑ Confluence  1,512  │  │ │ Context: This chunk is from the Payments scaling runbook…     │
│ ☐ Git           246  │  │ Scale to 5 replicas if queue depth exceeds 10k…  (highlighted)  │
│ SOURCE  (top 3 + more)│ │ Show surrounding chunks · Open in Confluence ↗                  │
│ FILE TYPE            │  ├──────────────────────────────────────────────────────────────────┤
│ TITLE CONTAINS [   ] │  │ …                                                                │
└──────────────────────┘  └──────────────────────────────────────────────────────────────────┘
```

## Behaviour

* **Query**: Enter or the Search button runs `POST /search {query, collection_ids, mode, top_k, filters}`.
  The mode defaults to Hybrid. `top_k` defaults to 10, and "Show more" raises it in steps up to the
  platform `search_max_top_k`.
* **Collection picker** (UX-10): see [design-system.md](../design-system.md#collection-picker-ux-10).
  Default scope is the user's last scope, or *All readable collections* on first visit. Empty selection
  means all readable.
* **Facets** (F-5): `GET /search/facets?collection_ids=…` loads when the scope changes, not on every
  query. The counts are document counts in scope. Sources show the top 5 plus "Show n more" with a
  search box.
* **Filters** (F-1) apply immediately and re-run the query. Values within a facet combine with OR, and
  facets combine with AND. "Updated" presets map to `updated_after`. "Custom" opens a two-date picker.
  Title contains is debounced at 300 ms.
* The **result summary line** restates the active scope and filters in words, so a screen reader or a
  screenshot captures the whole context.
* **Hit anatomy** (R-5, CR-3): title (link to the document drawer), connector pill, heading path as
  breadcrumbs, updated date, score (monospace, muted, with a tooltip explaining RRF), then the **context
  line on its own** (italic, labelled "Context:"), then the chunk text with query terms highlighted.
  "Show surrounding chunks" expands the neighbours inline (`GET /documents/{id}/chunks?from_ordinal=`).
* **Ask in chat** carries the query, scope and filters over to Chat.
* URL state: `?q=…&c=<ids>&mode=hybrid&updated=90d&connector=confluence`, which is shareable. Collection
  ids the recipient can't read are silently dropped (F-3).

## States

| State | Treatment |
|---|---|
| Before first query | Recent searches (from `query_logs` for this user) plus "Try: …" examples drawn from favourite collections |
| Searching | Skeleton hits. The previous results stay dimmed underneath |
| No results | "No results in 2 collections with 'Updated: last 90 days'." Then the actions [Remove date filter] [Search all collections] |
| Filtered too far | When a facet yields 0, its options are disabled with count 0 rather than hidden |
| Error | Danger callout with Retry. Query and filters are kept |
| Embedding not configured (S-15) | "Search isn't ready yet. An admin needs to finish setup." Admins get a link to setup |
