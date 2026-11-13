# Flow 7: Indexing settings with impact preview

Spec: UX-13, §11.1 PS-1 to PS-6, I-8, ST-4, CH-2/CH-3, §10.6 CR-1 to CR-8, E-8. Mockup:
[`../indexing-settings.html`](../indexing-settings.html).

## Goal

An editor can change how a collection is indexed **knowing beforehand** what it will cost (time, LLM
spend, whether sources are re-fetched), with no risk to search while it rebuilds.

## Wireframe

```
[… tabs …] [Indexing settings •]
┌ Chunking  [• Changed] ─────────────────────────────┐ ┌ ◎ What saving will do ──────────────────┐
│ Maximum chunk size ──●───────────── [800] chars    │ │ ≡ Re-chunk 1,486 documents       ~4 min │
│ Was 1,000 · allowed 200–8,000. Smaller = precise…  │ │   From stored chunks; nothing fetched.  │
│ Minimum section size [200] chars                   │ │   ~22,900 → ~28,400 chunks              │
│ (●) Include heading path in embeddings and results │ │ ✦ Generate context for ~28,400  ≈ $9.30 │
├ Context line  [• Changed] ─────────────────────────┤ │   claude-haiku-5-5 · ~410 tokens each   │
│ (●) Add a one-sentence context (was off)           │ │ ▣ Re-embed ~28,400 chunks       ≈ $0.12 │
│ Model [Platform default · haiku ▾] From [window 2▾]│ │   unchanged text comes from cache       │
│ [👁 Preview on a chunk]                            │ │ ✓ No re-fetch from sources              │
├ Images (vision) ───────────────────────────────────┤ │   Only a parser change would need one.  │
│ Model · Max images · Min px · Keep figure images   │ ├─────────────────────────────────────────┤
├ Parsing ───────────────────────────────────────────┤ │ ⓘ Search keeps using current chunks…    │
│ Parser [Automatic ▾] (needs full re-fetch)         │ │   Total ≈ 12 min, ≈ $9.42               │
│ Clean-up [✓ Basic clean-up]                        │ │ ☑ Run "On-call & incidents" eval after  │
├ Search ────────────────────────────────────────────┤ ├─────────────────────────────────────────┤
│ Full-text language [English ▾]                     │ │ [Discard]          [Save and rebuild]   │
└────────────────────────────────────────────────────┘ └─────────────────────────────────────────┘
```

## Behaviour

* The form is generated from the plugins' JSON Schemas (PS-2). Sections follow PS-1: *Chunking*,
  *Context line*, *Images (vision)*, *Parsing*, *Search*.
* Each changed field shows "Was …" under it, and its section gets a **Changed** pill. The tab label gets
  a dot.
* **The impact panel updates live**, 400 ms after the last edit, through
  `POST /collections/{id}/pipeline/preview` with the draft `pipeline`. The response lists rebuild steps
  with counts, time and cost.

  | Change | Rebuild shown | Fetch? |
  |---|---|---|
  | chunk max/min, heading path | Re-chunk *n docs* (+ re-embed, + re-context if on) | no |
  | context on / model / prompt / scope / window | Re-context *n chunks* + re-embed | no |
  | context off | Clear context + re-embed | no |
  | vision model / prompt / min px / max images, keep figures | Re-parse *n docs* (force resync) | **yes**, and upload sources need re-upload: "*3 upload sources (212 files) can't be re-parsed without re-uploading*" |
  | parser / transformers | Re-parse | **yes** (same warning) |
  | full-text language | Rebuild full-text index | no, no LLM cost |

* **Cost** (CR-8) appears only when the model has a price configured. Otherwise the panel shows "Cost
  unknown. Set a price on the model to see estimates" with a link for admins.
* **CH-3 guard**: if `max_chars` plus the context line could exceed the embedding model's input limit,
  the field shows a warning: "Chunks this size can exceed text-embedding-3-small's 8,191-token limit and
  will be split further."
* Validation (422) shows inline. Saving is blocked while there are errors.
* **Save and rebuild** PATCHes the pipeline, which enqueues the jobs in the same transaction. The page
  then switches to Overview with the progress card (PS-3). PS-4: the panel copy states that search keeps
  serving previous chunks.
* The checkbox "Run *<set>* evaluation when done" (E-8) is checked by default when the collection has an
  eval set, and is hidden otherwise ("Create an evaluation set to measure this change").
* **Preview on a chunk** opens the Context line try-it panel ([flow 2](02-model-providers-and-roles.md))
  as a drawer, scoped to this collection and using the draft settings.
* Leaving with unsaved changes prompts "Discard changes to indexing settings?".
* If a rebuild is already running, the panel says "A re-chunk is in progress (55%). Saving now cancels it
  and starts a new one from the current chunks." It still allows saving.
* The same form (without the impact panel, with "Applies to new collections") is used for Admin →
  Indexing defaults.
