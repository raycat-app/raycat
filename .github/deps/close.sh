#!/usr/bin/env bash
# Номера issue бота исправления зависимостей (по одному на строку), которые нужно закрыть, когда
# cargo deny на main снова проходит. Заголовки совпадают с теми, под которыми бот заводит issue
# в deps-fix.yml.
#   close.sh <список issue в JSON: gh issue list --json number,title>
set -euo pipefail

issues=${1:?не указан файл со списком issue}
[ -f "$issues" ] || { echo "ОШИБКА: нет файла со списком issue: $issues" >&2; exit 1; }

jq -r '
  .[]
  | select(.title == "Зависимости: PR бота не прошёл проверки"
      or .title == "Зависимости: не удалось исправить автоматически")
  | .number
' "$issues"
