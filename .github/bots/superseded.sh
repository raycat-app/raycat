#!/usr/bin/env bash
# Открытые PR бота на более старые версии того же клиента или xray, которые закрываются, когда бот
# начинает обновление до новой версии. Версии сравниваются по номерам (sort -V), префикс ветки
# отделяет клиента (приложение и платформа) или xray.
#   superseded.sh <новая версия> <префикс веток> < <JSON: gh pr list --json number,headRefName>
# Печатает по строке «номер<TAB>ветка» на каждый PR, который нужно закрыть.
set -euo pipefail

new=${1:?не указана новая версия}
prefix=${2:?не указан префикс веток}
[[ $new =~ ^[0-9A-Za-z._-]+$ ]] || { echo "ОШИБКА: недопустимая версия: $new" >&2; exit 1; }

number_of() {
  local tag=$1
  printf '%s' "${tag#"${tag%%[0-9]*}"}"
}

newv=$(number_of "$new")
[ -n "$newv" ] || { echo "ОШИБКА: в версии нет номера: $new" >&2; exit 1; }

jq -r --arg prefix "$prefix" '.[] | select(.headRefName | startswith($prefix)) | "\(.number)\t\(.headRefName)"' |
while IFS=$'\t' read -r number branch; do
  v=$(number_of "${branch#"$prefix"}")
  [ -n "$v" ] || continue
  if [ "$v" != "$newv" ] && [ "$(printf '%s\n%s\n' "$v" "$newv" | sort -V | tail -n 1)" = "$newv" ]; then
    printf '%s\t%s\n' "$number" "$branch"
  fi
done
