# CLAUDE.md — ACP-A2A_gateway

Read [`AGENTS.md`](AGENTS.md): it holds the rules for this repository — the GitNexus gate
(`impact` before any edit, `rename` never find-and-replace, `detect_changes` before commit),
build/test gates, the English-comments policy, the docs language scheme, and the release
procedure. This file exists only because some tools look for `CLAUDE.md` specifically.

Quick orientation: `README.md` (directions, structure, run) · `CHANGELOG.md` (what shipped) ·
`docs/06-gateway-guide.md` (full guide; `-ru`/`-uk` next to it) ·
`docs/skills/gateway-client/SKILL.md` (how an agent talks to the gateway) ·
`config.example.yaml` (what is configurable).

> Both files are published from an out-of-repo overlay
> (`_doc_overlay/ACP-A2A_gateway/`) by `scripts/publish-docs.sh`; do not let code-indexing
> tooling append generated blocks to the tracked copy.
