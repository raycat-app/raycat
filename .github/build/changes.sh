#!/usr/bin/env bash
# Меняет ли запуск CI файлы, от которых зависит программа: печатает `code=true` или
# `code=false` для $GITHUB_OUTPUT.
#   changes.sh <событие> <база> <коммит>
# Только документация (*.md) и картинки README (.github/assets) — `false`: сборки, тесты
# и e2e можно пропустить. Ручной запуск, пустая или неизвестная база — всегда `true`.
set -euo pipefail

event=${1:-}
base=${2:-}
head=${3:-HEAD}

code() {
  echo "code=$1"
  exit 0
}

[ "$event" = workflow_dispatch ] && code true
case "$base" in
  "" | 0000000000000000000000000000000000000000) code true ;;
esac
git cat-file -e "$base^{commit}" 2>/dev/null || code true

while IFS= read -r path; do
  case "$path" in
    "") ;;
    *.md | .github/assets/*) ;;
    *) code true ;;
  esac
done < <(git diff --name-only "$base" "$head")
code false
