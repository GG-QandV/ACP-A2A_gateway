#!/usr/bin/env bash
# scripts/publish-docs.sh — кладёт в GitHub-репозиторий ВЕРНЫЕ AGENTS.md / CLAUDE.md,
# которые живут вне репозитория (в оверлее), и прячет локальный шум индексаторов.
#
#   ./scripts/publish-docs.sh              # показать, что изменится (dry-run)
#   ./scripts/publish-docs.sh --apply      # опубликовать через Contents API
#   ./scripts/publish-docs.sh --status     # какие sha сейчас в репо и в оверлее
#
# Зачем: gitnexus/аналогичные индекаторы дописывают в AGENTS.md/CLAUDE.md свой блок
# <!-- gitnexus:start --> ... <!-- gitnexus:end -->. В истории репо этот шум не нужен,
# поэтому отслеживаемая копия идёт из оверлея, а локальный файл остаётся свободным для
# правок инструментом (skip-worktree).
set -euo pipefail

OWNER_REPO="GG-QandV/ACP-A2A_gateway"
FILES=(AGENTS.md CLAUDE.md)
OVERLAY="/home/gg/projects/_doc_overlay/ACP-A2A_gateway"
MODE="${1:---dry-run}"

need() { command -v "$1" >/dev/null || { echo "нет $1 в PATH" >&2; exit 2; }; }
need gh; need base64; need git

sha_in_repo() {
  gh api "repos/$OWNER_REPO/contents/$1" --jq .sha 2>/dev/null || echo ""
}

case "$MODE" in
  --status)
    for f in "${FILES[@]}"; do
      printf "%-12s repo sha=%-10s overlay: %s\n" "$f" "$(sha_in_repo "$f" | cut -c1-8)" \
        "$( [ -f "$OVERLAY/$f" ] && echo "есть ($(wc -l < "$OVERLAY/$f") строк)" || echo НЕТ )"
    done
    ;;
  --apply)
    [ -d "$OVERLAY" ] || { echo "нет оверлея $OVERLAY" >&2; exit 1; }
    for f in "${FILES[@]}"; do
      src="$OVERLAY/$f"; [ -f "$src" ] || { echo "пропуск: нет $src" >&2; continue; }
      sha="$(sha_in_repo "$f")"
      args=(-f message="docs($f): publish from overlay (no generated tool blocks)"
            -f content="$(base64 -w0 "$src")" -f branch=main)
      [ -n "$sha" ] && args+=(-f sha="$sha")
      gh api -X PUT "repos/$OWNER_REPO/contents/$f" "${args[@]}" \
        --jq '"опубликовано: " + .content.path + " @ " + .commit.sha[0:8]'
      git fetch --quiet origin
      cp "$src" "$f"                                  # локальная копия = опубликованная
      git update-index --skip-worktree "$f" 2>/dev/null || true   # шум индексатора не должен попадать в diff
      echo "  skip-worktree включён для $f (снять: git update-index --no-skip-worktree $f)"
    done
    # только что опубликованные файлы локально ещё untracked — подтягиваем их в индекс,
    # иначе git pull перезапишет их молча, а флаг skip-worktree спадёт
    git pull --ff-only --quiet origin main 2>/dev/null || echo "  пулл пропущен (не ff-only) — обнови вручную"
    for f in "${FILES[@]}"; do
      [ -f "$f" ] && git update-index --skip-worktree "$f" 2>/dev/null || true
    done
    ;;
  --dry-run|"")
    for f in "${FILES[@]}"; do
      src="$OVERLAY/$f"
      if [ ! -f "$src" ]; then
        echo "пропуск: нет $src"
        continue
      fi
      cur="$(sha_in_repo "$f")"
      new="$(base64 -w0 "$src" | md5sum | cut -c1-8)"
      printf "%-12s в репо sha=%-10s | оверлей %3s строк, md5=%s\n" \
        "$f" "${cur:-<нет файла>}" "$(wc -l < "$src")" "$new"
      [ -z "$cur" ] && echo "   -> будет СОЗДАН" || echo "   -> будет ЗАМЕНЁН (если содержимое отличается)"
    done
    echo "dry-run: ничего не отправлено; для публикации — $0 --apply"
    ;;
  *) echo "неизвестный режим '$MODE' (нужно: --dry-run | --apply | --status)" >&2; exit 2 ;;
esac
