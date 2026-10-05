#!/usr/bin/env bash
# Список изменений в Markdown, сгруппированный по типу коммитов, от новых к старым.
# Коммиты (JSON Lines из collect.sh) читаются из stdin; в текст попадают только заголовки.
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
jq -rs -L "$dir" -f "$dir/notes.jq"
