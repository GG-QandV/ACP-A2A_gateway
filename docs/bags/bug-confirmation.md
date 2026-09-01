> **Language:** English · [Русская версия](bug-confirmation-ru.md)

Context understood: this is a bug report about our gatewayd, plus important operational constraints of the site — building on the host is excluded, prebuilt binaries only, the config is already deployed with a workaround (raw tokens instead of `{env:...}`).

## Bug confirmation

You're right, and it's visible directly in the `main.rs` code we wrote:

rust

`fn build_registry(raw: &RawConfig) -> Registry {     let tokens: HashSet<String> = raw.tokens.iter().cloned().collect();  // ← raw, without resolve    ...    RawAgentEntry::Stdio { command, cwd, env } => Transport::Stdio {        env: env.iter().map(|(k, v)| (k.clone(), resolve_env_placeholders(v))).collect(),  // ← present here    },`

`resolve_env_placeholders` is called only for `env:` entries inside `agents:`, while `tokens: Vec<String>` is copied as-is via `.cloned()`. If the config says `tokens: ["{env:GATEWAY_TOKEN}"]`, the literal string `"{env:GATEWAY_TOKEN}"` ends up in `Registry.tokens` instead of the variable's value — `check_token` would compare the incoming Bearer against this garbage and always reject a valid token.

## Patch

gatewayd_main_tokens_fix

Rust

## Important caveat about `resolve_env_placeholders` — check before applying the patch

The function, as we wrote it, contains a silent fallback:

```rust
fn resolve_env_placeholders(value: &str) -> String {
    if let Some(var_name) = value.strip_prefix("{env:").and_then(|s| s.strip_suffix('}')) {
        std::env::var(var_name).unwrap_or_default()  // ← if the variable is missing — empty string
    } else {
        value.to_string()
    }
}
```

If `GATEWAY_TOKEN` is not exported in the gatewayd process environment at startup, `tokens` gets an **empty string** instead of an error — and then `check_token("")` will either falsely accept an empty Bearer, or (more likely) simply never match any real token, but without any explicit message that the variable was not found. This is a separate, quieter bug next to the one you found — it's worth replacing `unwrap_or_default()` with an explicit error at startup right away, if you're going to fix it.

## How to apply this given the "prebuilt binaries only" constraint

Since cargo/rust were deliberately removed from the host, the patch to `main.rs` must be built **not on this host** — on CI or another machine with the toolchain — and only the new `gatewayd` binary delivered to `/srv/gateway`. Safe rollback order:

1. Build the patched `gatewayd` in a different environment.
2. On the host: `systemctl stop gatewayd` (or however you run it), replace the binary, restore `config.yaml` to `tokens: ["{env:GATEWAY_TOKEN}"]` instead of raw values, and make sure `GATEWAY_TOKEN` really is in the unit's `Environment=`/`.env`.
3. Start up, check the logs for unexpected empty tokens (see the caveat above) — it's worth temporarily adding a `tracing::warn!` log on an empty resolve, so you don't hit a silent failure of all clients.
4. Only after verification — restore `chmod 600` on the config without plaintext tokens inside.

## Networking clarification — not a bug, but worth confirming intent

`http_listen: 172.17.0.1:8348` — this is the IP of the `docker0` bridge, i.e. the HTTP transport (direction 4, A2A client→ACP agent, plus A2A↔A2A passthrough) listens only on the docker bridge, unreachable from `127.0.0.1` or an external interface directly — Traefik must be a container in the same docker network to reach it. TCP on `127.0.0.1:8347` is the opposite: reachable only locally, ACP clients outside the host won't get to it at all. If this is a deliberate topology (Traefik is the only external entry point, the TCP transport is used only by local processes on the host) — that's fine and matches the "token = allow/deny at entry" model from the spec. If the TCP direction should also be reachable from outside — that's a separate edit of `listen:` in the config, unrelated to the token bug.
