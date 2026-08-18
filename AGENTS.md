# GitNexus Gate — BINDING (для всех агентов)

Репозиторий проиндексирован GitNexus как **`ACP-A2A_gateway`**. Код понимаем через граф, а не через grep.

**MANDATORY (перед правкой любого символа):**
- [ ] `impact(target, direction="upstream")` → отчёт о blast radius (callers, affected processes, risk). Risk HIGH/CRITICAL → НЕ править, показать пользователю.
- [ ] Rename только через `rename` (dry_run сначала), НЕ find-and-replace.
- [ ] Перед изменением route-хендлера → `api_impact`.
- [ ] Перед коммитом → `detect_changes()` — проверить, что затронуты только ожидаемые символы/флоу.

**Как искать:**
- "Как работает X?" → `query(search_query="...")` — execution flows по смыслу, не файлы.
- Нужен полный контекст символа (callers/callees/флоу) → `context(name)`.
- "Что сломается?" → `impact`. "Почему падает?" → `trace` / `gitnexus-debugging`.
- Security-ревью → `explain(target)` (taint source→sink).

**Запрещено:**
- Правка символа без `impact`.
- Игнор HIGH/CRITICAL risk.
- `query` для поиска по имени (для этого grep/glob).
- `git add -A` (тянет Cargo.lock и docs/) — стейджить только нужные файлы.

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **ACP-A2A_gateway** (2900 symbols, 5630 relationships, 255 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> Index stale? Run `node .gitnexus/run.cjs analyze` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? `npx gitnexus analyze` (npm 11 crash → `npm i -g gitnexus`; #1939).

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows. For regression review, compare against the default branch: `detect_changes({scope: "compare", base_ref: "main"})`.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `query({search_query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `context({name: "symbolName"})`.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

## Never Do

- NEVER edit a function, class, or method without first running `impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit changes without running `detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/ACP-A2A_gateway/context` | Codebase overview, check index freshness |
| `gitnexus://repo/ACP-A2A_gateway/clusters` | All functional areas |
| `gitnexus://repo/ACP-A2A_gateway/processes` | All execution flows |
| `gitnexus://repo/ACP-A2A_gateway/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
