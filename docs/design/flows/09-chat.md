# Flow 9: Chat with citations and feedback

Spec: §13 Chat routes, E-6, F-1, S-15. Mockup: [`../chat.html`](../chat.html).

## Wireframe

```
┌──────────────────────────────────────────────────────────────┬──────────────────────────────┐
│ (DK) ┌ When should we scale the ledger worker, and is …?  ┐  │ Sources   6 retrieved · 3 cited│
│      └─────────────────────────────────────────────────────┘ │ ┌ [1] Payments · Scaling rb ─┐ │
│ (F)  Scale the ledger worker to 5 replicas when the queue    │ │ Operations › Autoscaling    │ │
│      depth stays above 10k for more than 5 minutes… [1]      │ │ "Scale to 5 replicas if…"   │ │
│      There is a hard limit: the writer must stay at 1 [1][2] │ └─────────────────────────────┘ │
│      In the August incident… [3]                             │ ┌ [2] Ledger worker design ──┐ │
│      gpt-4.1-mini · 6 chunks · 2.1 s     [Copy] [👍] [👎]   │ ┌ [3] Incident 2026-08-14 ───┐ │
│                          ┌ What went wrong? (optional) ───┐ │ ▸ 3 more retrieved, not cited │
│                          │ [Missing info][Wrong source]   │ │                              │
│                          │ [Outdated][Not grounded]       │ │                              │
│                          │ [comment textarea]             │ │                              │
│                          │ Shared with collection owners… │ │                              │
│                          │            [Cancel] [Send]     │ │                              │
│                          └────────────────────────────────┘ │                              │
├──────────────────────────────────────────────────────────────┤                              │
│ ┌ Ask a follow-up…                                        ┐  │                              │
│ │ [▭ 2 collections] [⛛ Updated · 90 days]   Enter send  [➤]│  │                              │
└──────────────────────────────────────────────────────────────┴──────────────────────────────┘
```

## Behaviour

* The answer **streams** in. Citations `[n]` are buttons. Hovering or focusing one highlights the
  matching source card on the right, and clicking scrolls to it. Source cards link to the document drawer
  and to the original (↗).
* The **scope and filter chips** in the composer reuse the search collection picker and filters. They
  persist for the conversation, and changing them applies from the next turn.
* **Feedback** (E-6): 👍 records immediately, shows a toast "Thanks", and the button stays pressed. 👎
  opens the popover with optional reason chips and a comment, then sends
  `POST /chat/{query_log_id}/feedback`. The disclosure line says who will see it. Either rating can be
  changed afterwards.
* The metadata line shows the model, the number of chunks used and the latency. In the source panel,
  "6 retrieved · 3 cited" separates what was retrieved from what the answer actually cited.
* History: `history_turns` from the chat role. Older turns stay visible but are marked "not sent to the
  model" past the limit.
* API-key callers can't use chat (SSO only). The page isn't shown in API-key contexts.

## States

| State | Treatment |
|---|---|
| Chat disabled (503) | Page shows "Chat isn't enabled. An admin can turn it on under Model roles → Chat." Search link |
| Model error (502) | The answer bubble turns into a danger callout: "The chat model didn't respond (timeout after 60 s). [Retry]". The retrieved sources still show |
| No relevant chunks | The answer says so plainly, and the source panel shows "Nothing relevant found in 2 collections" with [Widen to all collections] |
| Streaming | A stop button replaces send. `aria-live="polite"` announces completion, not every token |
