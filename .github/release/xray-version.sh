#!/usr/bin/env bash
# Версия xray по выводу `xray version` (например 26.10.21). Бинарник запускается, поэтому
# скрипт годится только для xray той же платформы, что и раннер.
#   xray-version.sh <файл xray>
set -euo pipefail

binary=${1:?не указан файл xray}
output=$("$binary" version)
version=$(sed -n '/^Xray /{s/^Xray \([0-9][0-9.]*\) .*/\1/p;q}' <<<"$output")
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "ОШИБКА: версия xray не распознана в выводе: $output" >&2
  exit 1
}
echo "$version"
