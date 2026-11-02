#!/usr/bin/env bash
# Номера открытых issue клиента, которые закрываются после слияния обновления профиля: заголовок
# «Не удалось обновить профиль <приложение> <платформа> до <версия>» с любой версией. Заголовок
# совпадает с тем, под которым client-update.yml заводит issue.
#   client-issues.sh <приложение> <платформа> <список issue в JSON: gh issue list --json number,title>
set -euo pipefail

app=${1:?не указано приложение}
platform=${2:?не указана платформа}
issues=${3:?не указан файл со списком issue}
[ -f "$issues" ] || { echo "ОШИБКА: нет файла со списком issue: $issues" >&2; exit 1; }

prefix="Не удалось обновить профиль $app $platform до "
jq -r --arg prefix "$prefix" '
  .[]
  | select(.title | startswith($prefix) and length > ($prefix | length))
  | .number
' "$issues"
