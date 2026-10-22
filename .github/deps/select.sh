#!/usr/bin/env bash
# Разбор отчёта cargo-deny (--format json, строки JSON на stderr) для бота исправления
# зависимостей. Учитываются только ошибки уровня error с кодами vulnerability, unsound и yanked.
# Печатает JSON {"specs","advisories","yanked","title"}: specs — пакеты вида имя@версия для
# cargo update -p, advisories — идентификаторы RustSec, yanked — имена отозванных крейтов,
# title — заголовок PR (пустой, если исправлять нечего).
#   select.sh <отчёт cargo-deny>
set -euo pipefail

report=${1:?не указан файл отчёта cargo-deny}
[ -f "$report" ] || { echo "ОШИБКА: нет файла отчёта: $report" >&2; exit 1; }

jq -n -R -c '
  def krates: [(.fields.graphs // [])[] | select(has("Krate")) | .Krate];
  [inputs | (try fromjson catch empty) | select(type == "object" and .type == "diagnostic")] as $all
  | [$all[] | select(.fields.severity == "error"
      and (.fields.code == "vulnerability" or .fields.code == "unsound" or .fields.code == "yanked"))] as $d
  | [$d[] | if (krates | length) > 0
            then (krates[] | "\(.name)@\(.version)")
            else (.fields.advisory.package // empty) end] | unique as $specs
  | [$d[] | (.fields.advisory.id // empty)] | unique as $advisories
  | [$d[] | select(.fields.code == "yanked") | krates[] | .name] | unique as $yanked
  | [if ($advisories | length) > 0 then ($advisories | join(", ")) else empty end,
     if ($yanked | length) > 0 then ("отозванные версии: " + ($yanked | join(", "))) else empty end] as $parts
  | {
      specs: $specs,
      advisories: $advisories,
      yanked: $yanked,
      title: (if ($d | length) == 0 then ""
              elif ($parts | length) == 0 then "fix: обновление уязвимых зависимостей"
              else "fix: обновление уязвимых зависимостей (" + ($parts | join("; ")) + ")" end)
    }
' "$report"
