#!/usr/bin/env bash
# Решение бота о его открытых PR: ждать или обновлять версию поверх. Свежий PR (моложе порога),
# у которого проверки не упали, ждёт. PR с упавшими проверками или не менявшийся дольше порога
# не блокирует: новый прогон обновит ту же ветку и тот же PR, и проверки запустятся заново.
#   pr.sh <текущее время, секунды от эпохи> <порог, секунды> < <JSON: gh pr list --json number,updatedAt,statusCheckRollup>
# Печатает «wait: …», если ждать, иначе «go: …»; дальше номера PR и причины.
set -euo pipefail

now=${1:?не указано текущее время}
max_age=${2:?не указан порог}
[[ $now =~ ^[0-9]+$ && $max_age =~ ^[0-9]+$ ]] || { echo "ОШИБКА: время и порог — целые секунды" >&2; exit 1; }

jq -r --argjson now "$now" --argjson max "$max_age" '
  def bad: ["FAILURE", "ERROR", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED", "STARTUP_FAILURE"];
  def failed: [.statusCheckRollup[]? | (.conclusion, .state)] | map(select(. != null)) | any(. as $r | any(bad[]; . == $r));
  def fresh: ($now - (.updatedAt | fromdateiso8601)) < $max;
  def why: if failed then "проверки упали" elif fresh then "свежий" else "не менялся дольше порога" end;
  [.[] | {number, waiting: (fresh and (failed | not)), why: why}] as $prs
  | if ($prs | length) == 0 then "go: открытых PR нет"
    elif any($prs[]; .waiting) then "wait: " + ([$prs[] | select(.waiting) | "#\(.number) \(.why)"] | join(", "))
    else "go: " + ([$prs[] | "#\(.number) \(.why)"] | join(", "))
    end
'
