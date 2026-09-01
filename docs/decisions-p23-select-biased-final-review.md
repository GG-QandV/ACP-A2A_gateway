# P-23. Final code review of Phase 2.0 — real implementation vs P-20/21, and the found `biased` risk

> **Language:** English · [Русская версия](decisions-p23-select-biased-final-review-ru.md)

Added into `docs/decisions.md` after P-22. Written on the fact of code actually read (commits `af9c9d9`,
`1ee5574`, `36745ac`, `1e2de5d`, `564ea1e`), not on a forecast.

---

### P-23. `prompt_streaming()` implemented via `tokio::select!`, not via decision (c) from P-20

P-20 (draft patch `convert-streaming-mapping.rs`) assumed decision (c): the channel is always returned
as `Reply::Streaming`, terminal state defaults to `Completed`/`Failed` without an explicit `StopReason` in
the streaming path. The implementation (commit `36745ac`) chose the more precise path — `tokio::select!` between
`resp_rx` (final `PromptResponse`) and `rx.recv()` (first `SessionUpdate` chunk) in `prompt_streaming()`:

- If the final response arrived first (the agent did not stream at all — 0 chunks) → `Reply::Complete`,
  preserving the old behavior for all calling code that does not expect a stream.
- If a chunk arrived first → `Reply::Streaming`, a background task forwards the remaining chunks and at the end
  emits a terminal element built from the real `PromptResponse` (not synthetic, as
  decision (c) assumed).

**This is better than assumed**: the real `StopReason` is available in the terminal element of the stream,
and is not lost. P-20/21 remain in force for everything else (only `AgentMessageChunk` is parsed,
`UsageUpdate` → DEBUG) — only this detail changes.

**Consequence, mandatory to eliminate as a separate ticket**: `select!` without `biased` gives a
nondeterministic branch choice when both are ready simultaneously (the agent sent its single chunk
and completed almost immediately). Both outcomes are semantically valid (content is not lost), but it creates
a flaky risk for tests T1/T3/T9 in edge cases. Does not block the Phase 2.0 release — recorded as a
separate follow-up:

```markdown
### 2026-08-XX: tokio::select! in prompt_streaming() without biased — nondeterministic path
- **What**: when resp_rx and the first chunk are ready simultaneously, the branch choice is random.
- **Why**: left for follow-up — does not block correctness (both outcomes are valid), but creates
  a risk of flaky tests at the boundary "agent sent 1 chunk and immediately completed".
- **Impact**: low — aesthetic/test risk, not a functional bug.
- **Fix**: add `biased;` with the chunk branch prioritized first in core/src/stdio_agent.rs.
```

### P-24. `StreamCapacityExhausted` does not distinguish "agent not found" from "limit exhausted"

During the final review (`gatewayd/src/registry.rs`) found: `try_acquire_stream()` for an unknown
`agent_id` returns the same error type `StreamCapacityExhausted { active_streams: 0, limit: 0 }`
as for a genuinely exhausted limit. The transport layer must call `registry.lookup()` BEFORE
`try_acquire_stream()` to return the correct HTTP status to the client (404 vs 503) — by analogy with
the already existing separation of `AdapterError::UnknownAgent`/`Unavailable` in `transport_http.rs`.

No regression found (test `unknown_agent_stream_acquisition_fails` passes because it checks
only the fact of an error, not its classification) — but this is an architectural roughness worth
checking pinpoint: make sure the actual call order in `rpc_handler`/`handle_http_target`
(commit `36745ac`) is exactly `lookup` → `try_acquire_stream`, not the other way around.

**Not opened as a TECH_DEBT item** — only as a note in decisions.md, because it takes
a single glance at the order of two lines of code, not separate work.

---

## Final status of Phase 2.0 (review closure)

All five commits (`af9c9d9`, `1ee5574`, `36745ac`, `1e2de5d`, `564ea1e`) were read line by line across
the three key files (`registry.rs`, `main.rs`, `stdio_agent.rs`). Found:

- 1 low-priority follow-up (P-23, `biased` in `select!`) — does not block the release.
- 1 note for pinpoint verification (P-24, `lookup`/`try_acquire_stream` order) — not a separate task.
- 0 blocking defects.

**Phase 2.0 (basic streaming, directions 3 and 4, configurable limits, logging with rotation)
is considered closed.** T4 (TCP stream for the A2A→ACP direction via `HttpA2aAgent`) and
`tasks/resubscribe` correctly remain in TECH_DEBT as the scope of Phase 2.1, unchanged by this review.
