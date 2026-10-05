#!/usr/bin/env bash
# Следующая stable-версия: коммиты (JSON Lines из collect.sh) читаются из stdin,
# первый параметр — версия прошлой stable без «v» (по умолчанию 0.0.0).
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
last=${1:-0.0.0}
[[ $last =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "ОШИБКА: версия «$last» не вида X.Y.Z" >&2
  exit 1
}

jq -rs -L "$dir" --arg last "$last" 'include "common"; next_version($last)'
