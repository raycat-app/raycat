#!/usr/bin/env bash
# Последний stable-тег vX.Y.Z, достижимый из <ссылки> (по умолчанию HEAD); пусто, если тегов нет.
# Dev-теги (vX.Y.Z-dev.N) не подходят.
set -euo pipefail

ref=${1:-HEAD}
git tag --list 'v[0-9]*' --merged "$ref" | { grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' || true; } | sort -V | tail -n 1
