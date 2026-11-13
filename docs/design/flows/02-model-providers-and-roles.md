# Flow 2: Admin → Model providers and Model roles

Spec: §5.4 MP-1 to MP-7, S-3, S-5, S-8, S-13, §10.6 CR-2/CR-8, §20.3. Mockups:
[`../admin-model-providers.html`](../admin-model-providers.html), [`../admin-model-roles.html`](../admin-model-roles.html).

## Model providers

### Wireframe

```
Model providers                                                           [+ Add provider]
How Fable reaches your LLM services. Assign models to jobs under Model roles.
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ [⌕ Filter by name]  (+ Kind) (+ Status)                                    4 providers  │
├──────────────────────┬───────────┬────────────────────┬────────┬──────────────┬────────┤
│ Name ↓               │ Kind      │ Used by            │ Models │ Last test    │ Calls 7d│
│ Anthropic            │ Anthropic │ [Context line]     │ 9      │ ✓ OK · 2 h   │ 214,880 │
│  Key ••••8fQa · upd… │           │                    │        │              │      ⋯ │
│ Local Ollama         │ Ollama    │ [Vision][2 coll.]  │ 6      │ ⚠ Slow 2.4 s │   8,310 │
│ OpenAI               │ OpenAI    │ [Embedding][Chat]  │ 87     │ ✓ OK · 5 min │ 1.9 M   │
│ LiteLLM gateway      │ LiteLLM   │ Not used           │ —      │ ⊗ 401        │       0 │
└──────────────────────┴───────────┴────────────────────┴────────┴──────────────┴────────┘
                                         ┌─ drawer: Local Ollama ────────────────── ✕ ─┐
                                         │ Name, Base URL, Bearer token (secret)       │
                                         │ Usable by                                   │
                                         │ ⚠ Reachable but slow…                       │
                                         │ Discovered models  (cached 4 min) ⟳ Refresh │
                                         │  model │ capabilities │ context │ price/1M  │
                                         │ + Add a model manually                      │
                                         │ Used by: Vision role, 2 collection overrides│
                                         │ [Delete…]              [Test] [Save changes]│
                                         └─────────────────────────────────────────────┘
```

### Behaviour

* **Add provider** opens the same drawer, starting with a kind picker (6 cards, as in setup step 1).
* The secret field follows S-3: "Set · ends in …8fQa · updated 12 days ago · **Replace**". Saving without
  replacing keeps the secret.
* **Discovered models** (MP-2) shows capability pills (embedding with dimension, chat, vision, unknown),
  the context window and an optional editable price per 1M tokens, which feeds cost estimates (MP-6, CR-8).
* **Delete** of a provider in use returns 409. The dialog lists the dependents (roles, collection
  overrides) with links and offers no delete button until they are reassigned (S-8). An unused provider
  needs type-to-confirm.
* **Disable** (row menu) keeps the config. Dependent roles show "Provider disabled" and indexing jobs wait.

## Model roles

### Wireframe

```
Model roles
[✓ Embedding] [✓ Chat] [✓ Vision] [✓ Context line]          ← tabs; ⚠ when not configured
┌ Calls 7d ┐┌ Tokens in/out ┐┌ Latency p50/p95 ┐┌ Est. cost 7d ┐   ← MP-6 per role
└──────────┘└───────────────┘└─────────────────┘└──────────────┘
┌ Default for new collections ─────────────┐┌ Try it ──────────────────── ✓ 0.38 s · 412/31 ┐
│ Provider [Anthropic ▾] Model [haiku ▾]   ││ Document [Payments · Scaling runbook ▾]       │
│ Context from (Neighbouring | Whole doc)  ││ Chunk    [#14 of 31 ▾]                         │
│ Neighbour window ──●──────── [2]         ││ 1 · PROMPT SENT                       [Copy]   │
│ Max tokens [50]   Concurrency [8]        ││ ┌ … [#12] dim  [#13] dim                     ┐ │
│ Prompt [textarea, variables listed]      ││ │   [#14] highlighted chunk                  │ │
│                                          ││ │   [#15] dim  [#16] dim  Write ONE sentence…│ │
│                                          ││ 2 · GENERATED CONTEXT LINE                     │
│                                          ││ ┌ ✦ This chunk is from the Payments …       ┐ │
│                                          ││ 3 · TEXT THAT WILL BE EMBEDDED                 │
│                                          ││ ┌ title / heading path / context / chunk    ┐ │
│                                          ││ 418 chars · ~105 tokens (limit 8,191)          │
│ • 1 unsaved change      [Discard][Save…] ││ [▶ Generate again]  Uses the unsaved settings  │
└──────────────────────────────────────────┘└───────────────────────────────────────────────┘
```

### Per-role "Try it" panels (MP-4)

| Role | Inputs | Output shown |
|---|---|---|
| Embedding | free text (default sample) | dimension, latency, first 8 values, model input limit |
| Chat | sample question plus an optional collection | streamed answer, tokens, latency |
| Vision | image upload (≤ 5 MB, not stored) | description, tokens, latency, and whether it would be skipped by `min_image_px` |
| Context line | document + chunk pickers (from any collection the admin can read) | **exact prompt** with the chunk highlighted and neighbours dimmed, the generated line, and the **final embed text** with its char/token count vs the embedding model's limit (CH-3 warning when over) |

The panel uses the **unsaved** form values, so admins can try before committing.

### Save behaviour

* **Embedding** → "Save…" opens the re-embed confirmation (S-13): "Re-embed 2.1 M chunks with
  text-embedding-3-large (3072 d). Estimated $41, about 3 h. Search keeps using the current model until you
  activate the new one." It then shows the blue/green progress card with an **Activate** button once the
  new space is complete.
* **Vision / Context line** → "Save…" applies the new default, then offers (MP-7): "14 collections use the
  default. Rebuild them now? Re-context: no fetch, ≈ $38. Vision: needs re-parse (force resync), uploads
  need re-upload." The choices are *Rebuild all*, *Choose collections…* and *Not now*. Collections with
  overrides are listed as unaffected.
* **Chat** saves immediately, with no rebuild.
* An unconfigured role tab shows an empty state ("Vision is off. Images in documents are skipped.
  [Set up]"), and the tab label carries ⚠ for Embedding only, since that one is required.
