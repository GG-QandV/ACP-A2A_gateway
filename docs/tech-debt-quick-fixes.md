# Quick TECH_DEBT fixes — from simple to complex

> **Language:** English · [Русская версия](tech-debt-quick-fixes-ru.md)

Three items, sorted by real implementation complexity, not by impact. For two of them a ready diff is
provided (files were read in full in this session, risk of guessing signatures is zero). For one (P-24) —
instructions without a guaranteed diff, because the current text of `transport_http.rs`/`transport_tcp.rs`
after the `try_acquire_stream` integration (commit `36745ac`) is not confirmed line by line — project files
lag behind `main` on GitHub and show the pre-streaming version with "Phase 1" stubs.

---

## 1. P-23: `biased` in `select!` (trivial, diff ready)

**File**: `core/src/stdio_agent.rs`, method `prompt_streaming()`.

**What to change**: add `biased;` as the first line inside the `tokio::select! { ... }` block between
`resp_rx` and `rx.recv()`.

```diff
         tokio::select! {
+            biased;
             resp = &mut resp_rx => {
                 let resp: PromptResponse = match resp {
                     Ok(Ok(value)) => serde_json::from_value(value)?,
                     Ok(Err(err_msg)) => anyhow::bail!("agent returned error for session/prompt: {err_msg}"),
                     Err(_) => anyhow::bail!("agent stdout closed before prompt response"),
                 };
                 self.updates.lock().await.remove(&session_key);
                 Ok(Reply::Complete(resp))
             }
             first = rx.recv() => {
```

**Branch order matters with `biased`**: `resp_rx` goes first in the original code, so with `biased`
the priority goes to exactly it — when both are ready simultaneously, the choice will always be in favor
of `Reply::Complete`, not `Reply::Streaming`. If the reverse priority is needed (prefer
`Streaming` if the agent has explicitly started streaming), you must **swap the branches**, not just
add `biased`:

```diff
         tokio::select! {
+            biased;
+            first = rx.recv() => {
+                let Some(first) = first else {
+                    self.updates.lock().await.remove(&session_key);
+                    anyhow::bail!("stream channel closed before any event");
+                };
+                // ...the rest of the first branch unchanged...
+            }
             resp = &mut resp_rx => {
                 let resp: PromptResponse = match resp {
                     Ok(Ok(value)) => serde_json::from_value(value)?,
                     Ok(Err(err_msg)) => anyhow::bail!("agent returned error for session/prompt: {err_msg}"),
                     Err(_) => anyhow::bail!("agent stdout closed before prompt response"),
                 };
                 self.updates.lock().await.remove(&session_key);
                 Ok(Reply::Complete(resp))
             }
-            first = rx.recv() => {
-                let Some(first) = first else { ... };
-                // ...
-            }
         }
```

**Recommendation**: the second variant (chunk branch first) — it matches the intent "if the agent started
streaming, trust that signal" (P-23 in decisions.md). Apply one of the two variants, add a regression test:

```rust
#[tokio::test]
async fn simultaneous_response_and_chunk_prefers_streaming_path() {
    // The mock agent sends 1 session/update and the final response almost
    // simultaneously (no delay between them) — with biased and the chunk branch
    // first the result must be deterministic: Reply::Streaming,
    // not a random choice between Complete/Streaming across repeated runs.
}
```

**Negative control**: remove `biased` — the test above should become flaky (not always red, but
unstable across multiple runs `cargo test -- --test-threads=1 --repeat 20`, if such an option is
available, otherwise just record it as a known risk in a comment before the fix).

---

## 2. Token hash: minimal step without a new dependency (small, diff ready)

TECH_DEBT formulates the full fix as "replace with HMAC when the threat model hardens" — this is consciously
deferred work, not urgent (impact: low, brute-forcing is not in the current threat model [file:49]). But there is
an intermediate step that removes the main weakness of `DefaultHasher` **without new dependencies and
without changing the format** of `Owner::Token { hash: u64 }` — which is critical, because `Owner` is already
serialized and stored in `StoredTask` (`docs/decisions.md`, P-11), and changing the format would require
a data migration.

**File**: `core/src/owner.rs`.

**Problem**: `DefaultHasher::new()` is a stable `SipHash13` without a random key. The hash of one and the
same token is **identical on every process start**, which theoretically allows
precomputing/caching collisions in advance (an attacker can offline pick a token B such that
`hash(A) == hash(B)` once, and it will work on any gateway run).

**Minimal fix**: `std::collections::hash_map::RandomState` — the same API (`Hasher`
trait), but with a random key generated **on every process start**. A collision precomputed
by the attacker in advance stops working after the next restart — the cost of brute-forcing grows from "once
offline" to "from scratch on every deploy".

```diff
 impl Owner {
     pub fn from_token(token: &str) -> Self {
         use std::hash::{Hash, Hasher};
-        let mut hasher = std::collections::hash_map::DefaultHasher::new();
+        // FIXED (TECH_DEBT: token hash): RandomState instead of
+        // DefaultHasher — the same low-level algorithm (SipHash), but
+        // with a random key per process start. Does not replace
+        // a full HMAC (see TECH_DEBT: "replace with HMAC when the
+        // threat model hardens" — remains future work), but
+        // eliminates collision precomputability across restarts without
+        // a single new dependency and without changing the Owner::Token format.
+        let mut hasher = OWNER_HASH_SEED.build_hasher();
         token.hash(&mut hasher);
         Owner::Token { hash: hasher.finish() }
     }
```

Plus a global `RandomState`, initialized once per process (not on every
`from_token` call — otherwise one and the same token would give **different** hashes during the process lifetime, which would
break all "same client = same owner" logic):

```rust
// Add at the top of the file, after the use-directives:
use std::collections::hash_map::RandomState;

/// One seed for the whole process — otherwise two consecutive
/// from_token("t-1") calls would give different hashes, and the "same client" check would always fail.
static OWNER_HASH_SEED: std::sync::LazyLock<RandomState> =
    std::sync::LazyLock::new(RandomState::new);
```

**Check the Rust version**: `LazyLock` was stabilized in Rust 1.80. The project already requires 1.80+
(`docs/06-gateway-guide.md`, §2: `rustc --version   # needs 1.80+`) — compatible without additional
dependencies like `once_cell`.

**Tests**: the existing 4 tests in `core/src/owner.rs` (`same_token_gives_same_owner`,
`different_tokens_give_different_owners`, `anonymous_never_equals_token_owner`,
`owner_survives_serde_roundtrip`) must stay green without changes — they check behavior
within a single process run, where the seed is fixed. Add one new test documenting
the behavior change between processes (not automatable in a unit test directly, but recorded
by a comment):

```rust
/// The hash of the same token changes BETWEEN process restarts
/// (RandomState, not DefaultHasher) — within a single run (as in this
/// test) it is stable, which is exactly what owner comparison requires.
/// The change between processes is not checked by a unit test (needs a separate
/// process), but is documented as expected behavior.
#[test]
fn hash_is_stable_within_process_lifetime() {
    let a = Owner::from_token("t-1");
    let b = Owner::from_token("t-1");
    assert_eq!(a, b, "within one process the hash of the same token must be stable");
}
```

**TECH_DEBT.md update**: the item is not fully closed (HMAC remains future work), but the
wording is updated:

```markdown
### 2026-08-09: token hash — `std::hash::DefaultHasher` (partially closed)
- **What**: the token hash was stable across process restarts (DefaultHasher without a random seed).
- **Partially closed (2026-08-XX)**: replaced with `RandomState` — a random seed on every process
  start, eliminates precomputable collisions across deploys. The format `Owner::Token { hash: u64 }`
  did not change — data in `StoredTask` requires no migration.
- **Remaining**: not a cryptographic hash within a single run — if the threat model hardens
  (e.g. real-time token brute-forcing against a live process appears), a
  full HMAC with a secret key from config/env is needed, not just a random seed.
- **Impact**: low (unchanged)
- **Fix (remaining)**: HMAC-SHA256 with a key from `{env:GATEWAY_HMAC_KEY}` when the threat model hardens.
```

---

## 3. P-24: `lookup` before `try_acquire_stream` (small-medium, instructions without a diff)

**I am not giving a ready diff** — the reason is honest: project files in this session show the version
of `transport_http.rs`/`transport_tcp.rs` **before** the `try_acquire_stream` integration ("Phase 1" stubs
are still in place in the text that was read), although commit `36745ac` on GitHub has already done this integration.
Writing a diff against unconfirmed text risks repeating the same mistake that already happened in this
conversation with a name mismatch (`RpcOutcome` vs the actual `DispatchResult`).

**What needs to be done** (instructions for verification and pinpoint edit):

1. Open the current `gatewayd/src/transport_http.rs` (after commit `36745ac`) in the real
   repository — not through this chat.
2. Find the place where `registry.try_acquire_stream(&agent_id)` is called — per the commit message of
   `36745ac`, that is `rpc_handler` (JSON-RPC) and `rest_send_message_core` (SDK REST).
3. Check: does the `try_acquire_stream` call come **after** `get_or_spawn_adapter`/`registry.lookup`
   (i.e. after it is already confirmed that `agent_id` exists), or before?
   - If **after** lookup — everything is correct, `StreamCapacityExhausted` for a nonexistent
     agent physically cannot happen (agent_id was already checked earlier), the task is closed, you can
     just add a comment to `docs/decisions.md` (P-24) "checked — order is correct".
   - If **before** lookup — a pinpoint edit is needed: move the `try_acquire_stream` call after a
     successful `get_or_spawn_adapter`, and distinguish in error handling: `AdapterError::UnknownAgent`
     (404) must be returned before the gateway even tries to take a permit.
4. Check the same in `gatewayd/src/transport_tcp.rs`, `handle_http_target` — there `try_acquire_stream`
   per commit `36745ac` is called "in scope" through the new `HttpTargetParams struct".

**Why this is not a "ready diff"**: the fix itself (if it is needed) is moving one method-call line,
trivial in volume. But writing it now would mean guessing the exact variable name, the structure of
`HttpTargetParams` (mentioned in the commit but not seen line by line), and the exact error text,
which I have not confirmed by reading. Verification is 5 minutes of eyeballs in the real file; a wrong diff
based on outdated project files is a risk of breaking what already works.

**Criterion that no check is needed at all**: if the test `unknown_agent_stream_acquisition_fails` in
`gatewayd/src/registry.rs` is the only check for this case, and no HTTP/TCP integration test for
"a request to a nonexistent agent_id with an active stream limit returns 404, not 503" exists — it is
worth **adding** such a test as a minimum, even if the call order is already
correct, because otherwise a regression (if someone changes the order in a future refactor)
would not be caught.
