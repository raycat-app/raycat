#!/usr/bin/env bash
# Версия dev-выпуска для HEAD: следующая stable по коммитам с прошлой stable и «-dev.N»,
# N — номер запуска workflow (RUN_NUMBER). Результат — в $GITHUB_OUTPUT (по умолчанию stdout):
# version, tag, last_tag (прошлая stable), previous (прошлый dev-тег).
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
run_number=${RUN_NUMBER:?не задан RUN_NUMBER}
[[ $run_number =~ ^[0-9]+$ ]] || {
  echo "ОШИБКА: RUN_NUMBER не число: $run_number" >&2
  exit 1
}

last_tag=$(bash "$dir/stable-tag.sh" HEAD)
last=${last_tag#v}
next=$(bash "$dir/collect.sh" "$last_tag" HEAD | bash "$dir/version.sh" "${last:-0.0.0}")
version="${next}-dev.${run_number}"
previous=$(git describe --tags --abbrev=0 --match 'v*-dev.*' HEAD 2>/dev/null || true)

echo "Прошлая stable: ${last_tag:-нет}; прошлая dev: ${previous:-нет}; версия: $version" >&2
{
  echo "version=$version"
  echo "tag=v$version"
  echo "last_tag=$last_tag"
  echo "previous=$previous"
} >>"${GITHUB_OUTPUT:-/dev/stdout}"
