#!/usr/bin/env bash
# Сводка решения о продвижении (plan.json из plan.sh) в Markdown.
set -euo pipefail

plan=${1:?не указан plan.json}
jq -r '
  "## Продвижение в stable\n",
  (if .promote
   then "**Решение:** продвинуть `\(.tag)` как `v\(.version)`."
   else "**Решение:** не продвигать." end),
  "",
  "Причина: \(.reason)",
  "",
  "| Коммит | Класс | Срок, ч | В dev с | Заголовок |",
  "| --- | --- | ---: | --- | --- |",
  (.entries[]
   | "| \(.sha[0:7]) | \(.category) | \(.wait / 3600) | \(if .published then (.published | todate) else "ещё нет" end) | \(.subject | gsub("\\|"; "/")) |")
' "$plan"
