#!/usr/bin/env bash
# Версия Xray для обновления: самый новый по дате публикации релиз XTLS/Xray-core с тегом
# vX.Y.Z, у которого есть три архива linux и их .dgst. XTLS помечает все релизы предрелизами,
# поэтому флаг prerelease не учитывается. Печатает версию без v, если она новее текущей, иначе
# ничего. Явная версия выбирается вместо самой новой и не может быть старее текущей.
#   select.sh <JSON со списком релизов API> <текущая версия> [версия]
set -euo pipefail

releases=${1:?не указан файл со списком релизов}
current=${2:?не указана текущая версия}
requested=${3:-}

fail() {
  echo "ОШИБКА: $*" >&2
  exit 1
}

[ -f "$releases" ] || fail "нет файла релизов: $releases"
[[ $current =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "текущая версия не вида X.Y.Z: $current"
if [ -n "$requested" ]; then
  [[ $requested =~ ^v?[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "версия не вида X.Y.Z: $requested"
  requested=${requested#v}
fi

newer() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1)" = "$1" ]
}

lib='
def archives: ["Xray-linux-64.zip", "Xray-linux-arm64-v8a.zip", "Xray-linux-arm32-v7a.zip"];
def present($names; $name): any($names[]; . == $name);
def complete($names): all(archives[]; present($names; .) and present($names; . + ".dgst"));
def ready: .draft == false and (.tag_name | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$")) and complete([.assets[].name]);
'

if [ -n "$requested" ]; then
  tag=$(jq -r --arg tag "v$requested" "$lib"'[.[] | select(.tag_name == $tag and ready)] | .[0].tag_name // empty' "$releases")
  [ -n "$tag" ] || fail "релиза v$requested нет или он не готов (нужны три архива linux и их .dgst)"
else
  tag=$(jq -r "$lib"'[.[] | select(ready)] | sort_by(.published_at) | last | .tag_name // empty' "$releases")
  [ -n "$tag" ] || fail "среди релизов нет готового (нужны три архива linux и их .dgst)"
fi

version=${tag#v}
if newer "$version" "$current"; then
  echo "$version"
elif [ -n "$requested" ] && [ "$version" != "$current" ]; then
  fail "версия $version старее текущей $current, откат не выполняется"
fi
