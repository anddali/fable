# Fable portal design system

This is the visual and interaction reference for the portal (spec §20.1, UX-1 to UX-7). Its source of truth is
[`assets/fable.css`](assets/fable.css). The Svelte implementation in `ui/` (FAB-192) ports these tokens into
its own stylesheet. If you need a new token, add it in both places in the same PR and keep the names.

## Principles

1. **Configuration is a conversation.** Every form that touches an external system has a **Test** action
   that shows *what was checked* and *what to do next*. Avoid bare "Error 403" messages.
2. **Say the cost before the click.** Any action that triggers background work (re-chunk, re-context,
   re-embed, re-parse, eval run) states its scope (documents, chunks), its duration and its LLM cost first.
3. **Scale is the default.** Every list is paged on the server, can be filtered and linked, and stays fast
   with 1,000+ collections. Never render a checkbox per collection.
4. **Calm by default, loud when it matters.** The neutral palette carries the structure. Colour appears
   only for status, and status always comes with an icon and a word.
5. **Secrets are write-only.** After save, a secret shows as "Set · ends in …Q7mz · updated 3 days ago ·
   Replace". It is never shown again.

## Tokens

| Group | Tokens | Notes |
|---|---|---|
| Surfaces | `--bg`, `--surface`, `--surface-2`, `--surface-sunken`, `--overlay` | `bg` is the page, `surface` holds cards and inputs, `surface-2` is used for table headers, hovers and card footers |
| Lines | `--border`, `--border-strong` | `border-strong` is used for input and button outlines |
| Text | `--text`, `--text-muted`, `--text-faint`, `--text-on-accent` | All three text tokens meet AA (4.5:1) on `bg`, `surface` and `surface-2` |
| Brand | `--accent`, `--accent-hover`, `--accent-soft`, `--accent-soft-text`, `--focus` | Emerald, kept from today's UI. `focus` is blue so focus is never mistaken for selection |
| Status | `--ok`, `--info`, `--warn`, `--danger` and their `*-soft` backgrounds, `--warn-border` | Never used as a series colour |
| Chart | `--chart-1`, `--chart-grid` | Single-series charts only. See the evaluation page |
| Type | `--font` (Inter / system), `--mono`, `--fs-xs` 12 … `--fs-3xl` 30, `--lh` 1.5 | Body text is 14 px. Numbers in tables use `font-variant-numeric: tabular-nums` |
| Space | `--sp-1` 4 px … `--sp-10` 40 px | 4-point grid |
| Shape | `--r-sm` 4, `--r-md` 6, `--r-lg` 10, `--r-xl` 14 | |
| Elevation | `--shadow-1` (cards), `--shadow-2` (menus), `--shadow-3` (drawers, popovers, toasts) | |
| Layout | `--sidebar-w` 240, `--topbar-h` 56 | |

**Themes.** Light is the default. Dark applies through `[data-theme="dark"]` on `<html>`. The initial
value follows `prefers-color-scheme`, and the user's toggle (top bar) persists it. Dark values are chosen
separately and validated against dark surfaces; they are not an automatic inversion.

**Contrast.** `node docs/design/tools/check-contrast.mjs` checks every text/background pair used by the
mockups in both themes against WCAG 2.1 AA. Run it after you change a colour token.

## App shell (UX-2)

```
┌──────────────┬───────────────────────────────────────────────────────────────────────┐
│ [F] Fable prod│ Collections › Engineering handbook        [⌕ Search or jump to… Ctrl K] ☾ (DK) │
├──────────────┼───────────────────────────────────────────────────────────────────────┤
│ WORKSPACE     │                                                                       │
│ ⌂ Home        │  Page title                                        [secondary] [Primary]│
│ ⌕ Search      │  Lede: one sentence on what this page is for.                         │
│ ◌ Chat        │  ───────────────────────────────────────────────────────────────────  │
│ ▭ Collections │  Tabs (when the page has sections)                                    │
│ ⚿ API keys    │                                                                       │
│ ▯ Docs        │  Content                                                              │
│ ADMIN (admins)│                                                                       │
│ … 10 items    │                                                                       │
│ v2.0.0        │                                                                       │
└──────────────┴───────────────────────────────────────────────────────────────────────┘
```

* The sidebar collapses to icons below 1100 px. On phones it becomes a drawer, and only Search and Chat
  are optimised there (UX-6).
* Breadcrumbs appear on every page. The last crumb is `aria-current="page"`.
* The Admin section renders only when `/me` says the caller is an admin. The server still enforces it.
* A setup banner (`.banner`) sits above the top bar until an embedding model is configured (S-15).
* `Ctrl/Cmd+K` opens the command palette. It searches collections, pages and admin entities by name, and
  offers "Search documents for '…'".

## Components (UX-3)

| Component | Class(es) | Rules |
|---|---|---|
| Button | `.btn`, `.btn-primary`, `.btn-ghost`, `.btn-danger`, `.btn-sm`, `.btn-lg`, `.btn-icon` | One primary per view region. Icon-only buttons need `aria-label`. Destructive actions open a confirm dialog, and deleting a collection or connection requires typing its name |
| Field | `.field` > `.label` + control + `.help` / `.error-text` | The label is always visible. Help text includes an example. A 422 field error replaces the help text and sets `aria-invalid` |
| Input / select / textarea | `.input`, `.select`, `.textarea`, `.input-group` + `.addon` | 36 px high (44 px for `.input-lg` on primary inputs like search or paste-a-link) |
| Secret | `.secret` | Write-only display plus a "Replace" action, which swaps in a password input |
| Toggle | `.switch[role=switch][aria-checked]` | The label text describes the *on* state |
| Checkbox / radio | `.box` (`.on`, `.mixed`), `.radio` | |
| Choice cards | `.choice-cards` > `.choice[role=radio]` | For 2–4 options that need an explanation (auth method, scope, visibility) |
| Segmented | `.segmented` > `button[aria-pressed]` | For 2–4 short, mutually exclusive modes (Hybrid / Semantic) |
| Pill | `.pill` + `-ok`, `-info`, `-warn`, `-danger`, `-outline` | Status pills always carry an icon and a word. Running work uses the `refresh` icon with `.spin` |
| Tag | `.tag` | A removable chip in pickers |
| Callout | `.callout` + `-info`, `-ok`, `-warn`, `-danger` | Test results, explanations, inline errors with a next step |
| Tabs | `.tabs` > `a[role=tab][aria-selected]` | Counts in `.count`. A changed-but-unsaved tab shows `.changed-dot` |
| Card | `.card`, `.card-head`, `.card-body`, `.card-foot` | The footer holds the form actions of that card |
| Metric | `.card.metric` | Label, value, sub-line with delta (`.delta-up` / `.delta-down` carry ▲/▼ as well as colour) |
| Progress | `.progress[role=progressbar]` | Always paired with counts ("812 of 1,486") and an ETA |
| Data table | `.table-wrap` > `.toolbar`, `.bulkbar`, `table.data`, `.pager` | See "Lists" below |
| Drawer | `.drawer[role=dialog][aria-modal]` + `.scrim` | Edit forms for a row (connection, provider, add source). 560–600 px wide. Focus is trapped and `Esc` closes |
| Popover / menu | `.popover`, `.menu-item`, `.menu-group` | Picker lists, row action menus, feedback forms |
| Toast | `.toast[role=status]` | Raised when background jobs finish, with a link to details (UX-7) |
| Stepper | `.stepper` | For the setup wizard only |
| Code block | `.codeblock` with `.hl` / `.dim` | For prompts and embed-text previews |

## Patterns

### Lists that scale (UX-8 to UX-12)

* The toolbar holds a debounced search (`q`, 250 ms), then filter buttons (dashed outline when unset,
  solid with the value when set), a spacer, and **Columns** (visibility).
* Saved views appear as tabs above the table ("All", "Owned by me", "Favourites", "Needs attention").
* Selecting rows turns on the bulk bar, which shows the count and the bulk actions. The header checkbox
  selects the current page only, and a link offers "Select all 1,284 matching".
* The pager shows "1–25 of 1,284", a page-size choice of 25/50/100 (the API caps it at 100) and
  Previous/Next using cursors.
* Filters, sort, page size and cursor live in the URL query, so views can be linked and reloaded.
* Long views (documents, chunks, audit log) use the same pager, or virtualised scrolling with "Load more".
  They never render thousands of rows at once.
* Loading shows skeleton rows (`.skeleton`) at the final row height so the layout doesn't jump. An empty
  state says why it is empty and gives the next action ("No collections match 'plat'. Clear filters").

### Collection picker (UX-10)

A combobox of chips plus a typeahead input. The popover offers **All readable collections (n)**, then
matches with highlights, then **Favourites**, then **Recent**. Each row shows the document count and who
can see it. Keyboard: `↑/↓` moves, `Space` toggles, `Enter` toggles and keeps the popover open, `Esc`
closes, and `Backspace` on an empty input removes the last chip. It queries the server
(`/search/collections?q=`, paged), so the client never loads 1,000+ items. The same component is used by
search, chat, API-key scopes and eval sets.

### Forms (UX-4)

* Forms are generated from the plugin's JSON Schema. `title` becomes the label, `description` the help
  text and `examples` the placeholder. `format: password` renders the Secret component.
* Validation runs on blur in the client and on save in the server. A 422 `errors[field]` maps to the
  field's `.error-text`, and focus moves to the first invalid field.
* The unsaved-changes guard: each changed section gets a "Changed" pill, the tab shows a dot, the footer
  says "n unsaved changes", and leaving the page prompts.
* **Test before save**: forms for external systems have a Test button that uses the unsaved values
  (`POST …/test`). The result renders as a callout listing each check.

### Async work (UX-7)

Long jobs render as a card with a spinner icon, a title naming the job, "started n min ago by X", a
progress bar with counts and an ETA, and a line saying what stays available meanwhile ("Search keeps
serving the previous chunks"). Updates are announced through `aria-live="polite"`. When the job finishes,
a toast links to its details.

### Empty, loading and error states

Every data region designs all four states:

| State | Treatment |
|---|---|
| Loading | Skeleton rows and blocks at the final size |
| Empty (first use) | An illustration-free sentence plus the primary next action ("No sources yet. Add a source") |
| Empty (filtered) | "Nothing matches …" plus "Clear filters" |
| Error | Inline `.callout-danger` with a cause in plain words, a next step and **Retry**. Never a blank page |

## Accessibility (UX-5)

* Every control is reachable and operable by keyboard. Focus is visible (a 2 px blue ring), and the
  order follows the layout.
* Drawers and dialogs trap focus and return it to the trigger on close.
* Status is never conveyed by colour alone: pills and deltas carry an icon, a symbol or a word.
* Live regions announce job progress and test results.
* Charts carry an `aria-label` summary plus a "View as table" toggle.
* CI runs axe (Playwright + axe under `ui/tests`) on every key page in both themes. The mockups here
  pass axe's WCAG 2.0/2.1 A/AA rules (`tools/axe-check.cjs`).

## Responsive (UX-6)

The layout is designed at 1440 px. At 1280 px nothing changes. Below 1100 px the sidebar collapses to
icons, two-column layouts (settings + impact, chat + sources) stack, and the impact panel becomes a sticky
footer summary. Phones get single-column search and chat, and admin pages are read-only friendly.
