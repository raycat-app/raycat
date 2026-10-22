#!/usr/bin/env bash
# Отчёт cargo-deny для описания PR и issue: по одной строке на каждую ошибку уровня error
# (код, сообщение, крейты с версиями, идентификатор RustSec). Печатает Markdown-список.
#   report.sh <отчёт cargo-deny>
set -euo pipefail

report=${1:?не указан файл отчёта cargo-deny}
[ -f "$report" ] || { echo "ОШИБКА: нет файла отчёта: $report" >&2; exit 1; }

jq -n -r -R '
  def crates: [(.fields.graphs // [])[] | select(has("Krate")) | "\(.Krate.name) \(.Krate.version)"] | join(", ");
  [inputs | (try fromjson catch empty) | select(type == "object" and .type == "diagnostic" and .fields.severity == "error")] as $d
  | if ($d | length) == 0 then "- ошибок уровня error в отчёте нет"
    else $d[] | "- `\(.fields.code // "неизвестно")`: \(.fields.message); крейт: \(crates)\(if (.fields.advisory.id // "") != "" then " (" + .fields.advisory.id + ")" else "" end)"
    end
' "$report"
