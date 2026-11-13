# Flow 10: Running an evaluation

Spec: UX-13, §12.3 E-1 to E-8. Mockup: [`../evaluation.html`](../evaluation.html).

## Goal

An editor can tell, with numbers, whether a change to chunking, models or context made search better or
worse, and see *which questions* moved.

## Wireframe

```
[… tabs …] [Evaluation]
┌ Question sets      [+] ┐  On-call & incidents       [≡ Questions] (Hybrid|Semantic) [▶ Run evaluation]
│▌On-call & incidents    │  48 questions · created by Dana · searched as you, with your access
│  48 q · recall@5 0.86  │  ┌ ⟳ Run #7 in progress · queued after re-chunk     31 of 48 questions ┐
│  Onboarding FAQ        │  │ ███████████████████░░░░░░░░░                                       │
│  Architecture decisions│  └────────────────────────────────────────────────────────────────────┘
│ [⇪ Import] [Export]    │  ┌ Recall@1 ┐┌ Recall@5 ┐┌ Recall@10 ┐┌ MRR    ┐
├ Suggested questions ───┤  │ 0.62 ▲.04││ 0.86 ▲.06││ 0.92 ±0   ││ 0.71 ▲ │   vs run #5
│ 👎 Chat feedback · 2 h │  ┌ Recall@5 by run ────────────────────────────── View as table ┐
│ "When should we scale…"│  │ 0.9 ┤                                       ┌ Run #6 · 0.86 ┐  │
│ [+ Add to set]         │  │ 0.8 ┤                    ●───────●─────────●                  │
│ ⌕ Searched 41× / week  │  │ 0.7 ┤ ●──────●──────●                                         │
│ "pagerduty override"   │  │      #1     #2     #3   #4 context on  #5   #6 800 chars       │
│ [+ Add to set]         │  └───────────────────────────────────────────────────────────────┘
└────────────────────────┘  ┌ Compare [#5 · 1,000 chars ▾] › [#6 · 800 chars ▾]  ▲5 better ▼2 worse 41 same ┐
                            │ ▼ Worse  "Who approves a production database restore?"  expected: DB restore │
                            │ ┌ Run #5            rank 1 ┐ ┌ Run #6            rank 4 ┐                   │
                            │ │ 1. DB restore runbook ✓  │ │ 1. Change approval board │                   │
                            │ │ 2. Backup policy …       │ │ 4. DB restore runbook ✓  │                   │
                            │ Settings changed: chunk size 1,000 → 800 · Unchanged: hybrid, embedding…    │
                            └──────────────────────────────────────────────────────────────────────────┘
```

## Behaviour

**Sets (E-1)**
* The left rail lists sets for this collection (and multi-collection sets that include it). "+" creates
  one with a name and its collections, using the picker.
* **Questions** opens a paged table (question, expected documents as chips, notes, last result ✓/✗ with
  rank). Add/edit happens in a drawer. Expected documents are picked by searching inside the drawer and
  clicking "✓ Expected" on hits (E-2, one click).
* Import/Export as YAML/JSON (`POST /eval-sets/{id}/import`, `GET …/export`). Import shows a dry-run
  summary first ("42 new, 6 updated, 2 unknown document ids").

**Seeding (E-2)**
* "Suggested questions" come from `GET /collections/{id}/eval-suggestions`: thumbs-down chat feedback,
  then frequent queries. "Add to set" opens the question drawer pre-filled, with search results to pick
  the expected documents from.

**Running (E-3)**
* **Run evaluation** has a split-button menu with options: k values (default 1,5,10), mode, and filters.
  It calls `POST /eval-sets/{id}/runs`. A progress card shows "n of 48 questions". Runs use the
  requester's ACL, and the header says so ("searched as you, with your access").
* If a rebuild is in progress, the run is queued after it ("queued after re-chunk finished"), unless the
  user picks "Run now on current chunks".
* When the run finishes: a toast "Run #7: recall@5 0.86 (▲ 0.06)" with a link.

**Results (E-4, E-5)**
* Metric tiles show deltas against the previous run (▲/▼ plus colour plus number). A tile is neutral when
  the change is within ±0.01.
* "Recall@5 by run" is a single-series line chart. Points are annotated with the config change that
  distinguishes them from the previous run (from `config_snapshot`). It has a hover tooltip and a "View as
  table" toggle that shows every metric per run. Its `aria-label` summarises the series.
* **Compare** picks any two runs (defaulting to the last two). It shows better/worse/same counts as
  filter pills, then each moved question with both ranked result lists side by side, the expected
  document in bold with ✓, and its rank badge. The config diff is listed below ("Settings changed …
  Unchanged …").
* E-8: the collection Overview shows the latest metrics next to the settings that produced them.
  Indexing settings offers to "run this set after the rebuild".

## States

| State | Treatment |
|---|---|
| No sets | "Measure search quality before and after changes. Start with 20–50 real questions. [+ New set] [Import]" plus suggested questions |
| Set with no runs | Metrics area: "Not run yet. [▶ Run evaluation]" |
| Run failed | Danger callout with the error and Retry. Partial results are not shown as metrics |
| Expected document deleted | Question flagged "Expected document no longer exists" with [Pick again]. Excluded from metrics, and the count is shown |
