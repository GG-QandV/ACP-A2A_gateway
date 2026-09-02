# ACP-A2A_gateway — instructions for agents working in this repo

## What this project is

A gateway layer between **ACP agents** (Claude Code, Gemini CLI, Codex, Cline, opencode,
hermes) and **A2A clients**: two listeners (TCP + HTTP), four directions, token auth, per-client
conversation isolation, durable event buffer with resubscribe, journal, health monitoring, agent
approvals. Rust workspace of three crates (`protocol`, `gateway-core`, `gatewayd`); the version
lives once in `[workspace.package]` in the root `Cargo.toml` and is inherited by all of them.

## GitNexus gate — binding

The repository is indexed by GitNexus as **`ACP-A2A_gateway`**. Understand code through the
graph, not through grep.

Before editing any symbol:

- [ ] `impact(target, direction="upstream", repo: "ACP-A2A_gateway")` → blast radius (callers,
      affected processes, risk). Risk HIGH/CRITICAL → do **not** edit, show it to the human.
- [ ] Rename only via `rename` (`dry_run` first), never find-and-replace.
- [ ] Before changing a route handler → `api_impact`.
- [ ] Before committing → `detect_changes(repo: "ACP-A2A_gateway")` — confirm only the expected
      symbols/flows are affected.

How to search: "how does X work" → `query`; full context of a symbol → `context`; "what breaks"
→ `impact`; "why does it fail" → `trace`; security review → `explain` (taint source→sink).
`query` is not for name lookup — that is what grep/glob are for.

## Always

- Gate before committing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`,
  `cargo test --workspace` (integration tests spawn `target/debug/mock_acp_agent`, so run the
  whole workspace — `cargo test -p gateway-core --lib` alone fails on those four tests).
- Comments in code are English. Runtime string literals (protocol frames, log and error
  messages, test fixtures) are behaviour — do not translate them as a side effect of a comment pass.
- Documentation scheme: the English file carries the plain name, `-ru` (and `-uk` where it
  exists) are translations of it; every translation has a language-switcher line.
- Secrets are referenced by env variable name in `config.yaml` only. Never put a value in a
  command line, in a commit, or in a doc; host addresses and domains do not belong in tracked
  files either.

## Never

- `git add -A` — it drags in `Cargo.lock` churn and working documents; stage files by name.
- Tracking working material: `docs/Правки*`, `docs/суть+маркетинг/`, `docs/ToDo/`,
  `docs/bags/`, `config.yaml`, `README_ru.md`, `dist/` are git-ignored by design.
- Generated tool blocks in tracked files. `AGENTS.md`/`CLAUDE.md` are published from the
  out-of-repo overlay (`_doc_overlay/ACP-A2A_gateway/`) via `scripts/publish-docs.sh`; the local
  copy is free for tooling to rewrite because it is git-ignored and `skip-worktree`d.
- Pushing without an explicit human instruction; a commit is not a push.

## Release

1. Bump `version` in the root `Cargo.toml` only.
2. Add a `## [x.y.z]` section to `CHANGELOG.md`.
3. `git tag -a vX.Y.Z` and push the tag — `.github/workflows/release.yml` builds `gatewayd`
   for four targets (Linux, Windows, macOS arm64 + Intel, plus a universal macOS binary) and
   attaches them with `.sha256` sidecars to the GitHub Release.
4. Put the three install options and the checksum table into the release body; mark the
   superseded release in its title.
5. Force-pushing a rewritten `main` requires temporarily lifting branch protection — restore
   the byte-identical settings afterwards (`scripts/` snapshots from the last rewrite show how).
