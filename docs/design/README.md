# Portal designs

These are the wireframes and hi-fi mockups for the key portal flows of the Rust rewrite (spec
[§20](../RUST_REWRITE_REQUIREMENTS.md#20-portal-ui--ux), UX-13). Portal page tickets use them as their
reference: build to these, and if the implementation needs to differ, update the design in the same PR.

| What | Where |
|---|---|
| Design system (tokens, components, patterns, a11y) | [`design-system.md`](design-system.md), [`assets/fable.css`](assets/fable.css) |
| Wireframes and behaviour per flow | [`flows/`](flows/) (Markdown with ASCII wireframes, states, API calls) |
| Hi-fi mockups (static HTML, light and dark) | `*.html` in this folder |
| Rendered screenshots (1440 px, light and dark) | [`screens/`](screens/) |
| Tools (screenshots, contrast check, axe check) | [`tools/`](tools/) |

## Flows

| # | Flow | Wireframe and behaviour | Hi-fi mockup | Spec |
|---|---|---|---|---|
| 1 | First-run setup wizard | [flows/01](flows/01-first-run-setup.md) | [`setup.html?step=1…6`](setup.html) | UX-13, UX-14, S-15 |
| 2 | Admin → Model providers and Model roles (incl. context-line preview) | [flows/02](flows/02-model-providers-and-roles.md) | [`admin-model-providers.html`](admin-model-providers.html), [`admin-model-roles.html`](admin-model-roles.html) | §5.4 |
| 3 | Admin → Integrations | [flows/03](flows/03-admin-integrations.md) | [`admin-integrations.html`](admin-integrations.html) | §5.2, §5.3 |
| 4 | Paste-a-link Confluence source | [flows/04](flows/04-confluence-paste-a-link.md) | [`source-confluence.html`](source-confluence.html) | UX-13, S-14, CF-4 |
| 5 | Collections list at scale | [flows/05](flows/05-collections-list.md) | [`collections.html`](collections.html) | UX-8 to UX-12 |
| 6 | Collection detail tabs | [flows/06](flows/06-collection-detail.md) | [`collection-detail.html`](collection-detail.html) | §20.3 |
| 7 | Indexing settings with impact preview | [flows/07](flows/07-indexing-settings.md) | [`indexing-settings.html`](indexing-settings.html) | §11.1 |
| 8 | Search with collection picker and filters | [flows/08](flows/08-search.md) | [`search.html`](search.html) | §12.2, UX-10 |
| 9 | Chat with citations and feedback | [flows/09](flows/09-chat.md) | [`chat.html`](chat.html) | E-6 |
| 10 | Running an evaluation | [flows/10](flows/10-evaluation.md) | [`evaluation.html`](evaluation.html) | §12.3 |

## Screens

| Flow | Light | Dark |
|---|---|---|
| Setup 1 · provider | ![](screens/setup-1-provider-light.png) | ![](screens/setup-1-provider-dark.png) |
| Setup 2 · roles | ![](screens/setup-2-roles-light.png) | ![](screens/setup-2-roles-dark.png) |
| Setup 3 · integration | ![](screens/setup-3-integration-light.png) | ![](screens/setup-3-integration-dark.png) |
| Setup 4 · collection | ![](screens/setup-4-collection-light.png) | ![](screens/setup-4-collection-dark.png) |
| Setup 5 · source | ![](screens/setup-5-source-light.png) | ![](screens/setup-5-source-dark.png) |
| Setup 6 · first search | ![](screens/setup-6-search-light.png) | ![](screens/setup-6-search-dark.png) |
| Model providers | ![](screens/admin-model-providers-light.png) | ![](screens/admin-model-providers-dark.png) |
| Model roles · context line | ![](screens/admin-model-roles-light.png) | ![](screens/admin-model-roles-dark.png) |
| Integrations | ![](screens/admin-integrations-light.png) | ![](screens/admin-integrations-dark.png) |
| Paste-a-link source | ![](screens/source-confluence-light.png) | ![](screens/source-confluence-dark.png) |
| Collections | ![](screens/collections-light.png) | ![](screens/collections-dark.png) |
| Collection overview | ![](screens/collection-detail-light.png) | ![](screens/collection-detail-dark.png) |
| Indexing settings | ![](screens/indexing-settings-light.png) | ![](screens/indexing-settings-dark.png) |
| Search | ![](screens/search-light.png) | ![](screens/search-dark.png) |
| Chat | ![](screens/chat-light.png) | ![](screens/chat-dark.png) |
| Evaluation | ![](screens/evaluation-light.png) | ![](screens/evaluation-dark.png) |

## Viewing the mockups

Open any `*.html` file directly in a browser. There is no build or server; `file://` works. Add
`?theme=dark` or `?theme=light` to force a theme, or use the moon/sun button in the top bar.
`assets/shell.js` only injects the shared sidebar, top bar and icons. It is mockup scaffolding, not
production code.

Mockups use realistic but fictional data (Acme, people's names, counts) and show one representative
state per page. The other states (empty, loading, error, permission) are specified in each flow's
Markdown, and the paste-a-link drawer shows its error states inline as a design reference.

## Checks

```sh
node docs/design/tools/check-contrast.mjs          # WCAG 2.1 AA for every token pair, both themes
docs/design/tools/screenshots.sh                   # re-render screens/*.png (needs Chromium)
# axe (WCAG 2.0/2.1 A + AA) over every mockup, both themes; dependencies installed outside the repo:
mkdir -p /tmp/axe && (cd /tmp/axe && npm i puppeteer-core axe-core)
NODE_PATH=/tmp/axe/node_modules node docs/design/tools/axe-check.cjs
```

## Review

UX-13 requires the key flows to be reviewed **before** they are built. Each flow is reviewed in the PR
that adds or changes it. A page ticket starts once its flow's PR is approved. Review against:

- [ ] Covers the spec IDs listed for the flow, and nothing outside v1 scope (§19)
- [ ] Every external-system form has a test with per-check results and a next step on failure
- [ ] Every rebuild-triggering action states its scope, time, cost and whether sources are re-fetched first
- [ ] Lists are paged, filterable and linkable, and the picker works at 1,000+ collections
- [ ] Empty, loading, error and no-permission states are specified
- [ ] Both themes are legible, and the contrast and axe checks pass
- [ ] Copy is plain, says what happened and what to do, and never shows a secret

The usability test (UX-15, 3–5 people) uses flows 1, 4, 7 and 10 as its tasks once they are built. It is
tracked separately under FAB-150.

## Assumptions for implementers

The designs assume a few small API details that the spec implies but doesn't spell out. Confirm these
with the owning ticket:

- `/ui-config` exposes `setup: {complete, steps: {provider, embedding, integration, collection}}`, so the
  wizard can resume from server state (UX-14, S-15).
- `GET /collections` accepts `status`, `role`, `owner` and `connector` filters, returns per-row status
  aggregates, and supports a per-user favourite flag (UX-9, UX-10). Favourites need a small table or a
  user-preferences row.
- `POST /collections/{id}/pipeline/preview` returns a list of steps (`rechunk`, `recontext`, `reembed`,
  `reparse`, `fts`), each with `documents`, `chunks`, `est_seconds`, `est_cost_usd?` and
  `needs_fetch`/`upload_sources_affected`.
- `POST /collections/{id}/sources/test` accepts `{url}` and returns the resolved target, title, the
  space, child and page counts, and the chosen connection id. Error codes distinguish *unrecognised
  link*, *no usable connection for host* and *no access*.
- Chat feedback accepts an optional `reason` tag (`missing_info`, `wrong_source`, `outdated`,
  `not_grounded`) alongside the comment (E-6).
