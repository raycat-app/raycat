#!/usr/bin/env bash
# Тесты решения бота о его открытых PR и выбора issue клиента на выдуманных фикстурах, без сети.
# Запуск: bash .github/bots/test.sh
set -uo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0

check() {
  if [ "$2" = "$3" ]; then
    echo "ok    $1"
  else
    echo "FAIL  $1"
    echo "  ожидалось: $2"
    echo "  получено:  $3"
    failures=$((failures + 1))
  fi
}

failed() {
  if "$@" >/dev/null 2>&1; then
    echo ok
  else
    echo error
  fi
}

# Текущее время 2027-01-15T08:00:00Z, время обновления PR задаётся относительно него.
now=1800000000
window=43200
half_hour=2027-01-15T07:30:00Z
hour=2027-01-15T07:00:00Z
two_hours=2027-01-15T06:00:00Z
eleven_hours=2027-01-14T21:00:00Z
thirteen_hours=2027-01-14T19:00:00Z

pending='{"__typename":"CheckRun","name":"Итог","status":"IN_PROGRESS","conclusion":null}'
passed_status='{"__typename":"StatusContext","context":"Итог","state":"SUCCESS"}'
failed_status='{"__typename":"StatusContext","context":"Заголовок PR","state":"FAILURE"}'
failed_run='{"__typename":"CheckRun","name":"Итог","status":"COMPLETED","conclusion":"FAILURE"}'
cancelled_run='{"__typename":"CheckRun","name":"Итог","status":"COMPLETED","conclusion":"CANCELLED"}'

pr() {
  printf '{"number":%s,"updatedAt":"%s","statusCheckRollup":%s}' "$1" "$2" "$3"
}

decide() {
  printf '%s' "$1" | bash "$dir/pr.sh" "$now" "$2"
}

a=$(pr 5 "$hour" "[$pending]")
b=$(pr 5 "$hour" "[$failed_status]")
c=$(pr 5 "$hour" "[$failed_run]")
d=$(pr 5 "$hour" "[$cancelled_run]")
e=$(pr 5 "$thirteen_hours" "[$pending]")
f=$(pr 5 "$eleven_hours" "[$passed_status]")
g=$(pr 5 "$hour" "null")
h=$(pr 6 "$hour" "[$pending]")
i=$(pr 6 "$thirteen_hours" "[$pending]")
x=$(pr 5 "$half_hour" "[$pending]")
y=$(pr 5 "$two_hours" "[$pending]")

check "решение: открытых PR нет — обновлять" "go: открытых PR нет" "$(decide '[]' "$window")"
check "решение: свежий PR с идущими проверками ждёт" "wait: #5 свежий" "$(decide "[$a]" "$window")"
check "решение: свежий PR с упавшим статусом не ждёт" "go: #5 проверки упали" "$(decide "[$b]" "$window")"
check "решение: свежий PR с упавшим запуском не ждёт" "go: #5 проверки упали" "$(decide "[$c]" "$window")"
check "решение: отменённая проверка считается упавшей" "go: #5 проверки упали" "$(decide "[$d]" "$window")"
check "решение: PR старше порога обновляется, даже с идущими проверками" "go: #5 не менялся дольше порога" "$(decide "[$e]" "$window")"
check "решение: свежий PR с пройденными проверками ждёт слияния" "wait: #5 свежий" "$(decide "[$f]" "$window")"
check "решение: свежий PR без проверок ждёт" "wait: #5 свежий" "$(decide "[$g]" "$window")"
check "решение: упавший PR не блокирует, свежий ждущий блокирует" "wait: #6 свежий" "$(decide "[$b,$h]" "$window")"
check "решение: упавший и старый PR не блокируют" "go: #5 проверки упали, #6 не менялся дольше порога" "$(decide "[$b,$i]" "$window")"
check "решение, порог час (xray): PR в полчаса ждёт" "wait: #5 свежий" "$(decide "[$x]" 3600)"
check "решение, порог час (xray): PR старше часа обновляется" "go: #5 не менялся дольше порога" "$(decide "[$y]" 3600)"

check "аргументы: время не число — ошибка" error "$(failed bash "$dir/pr.sh" abc "$window" </dev/null)"
check "аргументы: нет порога — ошибка" error "$(failed bash "$dir/pr.sh" "$now" </dev/null)"
check "данные: не JSON — ошибка" error "$(printf 'не JSON' | failed bash "$dir/pr.sh" "$now" "$window")"

cat >"$work/issues.json" <<'EOF'
[
  {"number":11,"title":"Не удалось обновить профиль happ windows до desktop-v4.6.1"},
  {"number":12,"title":"Не удалось обновить профиль happ windows до desktop-v4.6.2"},
  {"number":13,"title":"Не удалось обновить профиль happ android до 4.9.0"},
  {"number":14,"title":"Не удалось обновить профиль incy android до 4.1.0"},
  {"number":15,"title":"Не удалось обновить xray до 26.10.31"},
  {"number":16,"title":"Не удалось обновить профиль happ windows до "},
  {"number":17,"title":"Не удалось обновить профиль happ windows-x до 1.0.0"}
]
EOF

check "issue клиента: все версии happ windows, пустой заголовок не закрывается" $'11\n12' "$(bash "$dir/client-issues.sh" happ windows "$work/issues.json")"
check "issue клиента: та же платформа, другая не затрагивается" 13 "$(bash "$dir/client-issues.sh" happ android "$work/issues.json")"
check "issue клиента: другой клиент не затрагивается" 14 "$(bash "$dir/client-issues.sh" incy android "$work/issues.json")"
check "issue клиента: нет совпадений — пусто" "" "$(bash "$dir/client-issues.sh" happ linux "$work/issues.json")"
check "issue клиента: нет файла — ошибка" error "$(failed bash "$dir/client-issues.sh" happ windows "$work/missing.json")"

cat >"$work/prs.json" <<'EOF'
[
  {"number":21,"headRefName":"bot/client-happ-windows-desktop-v4.6.1"},
  {"number":22,"headRefName":"bot/client-happ-windows-desktop-v4.7.0"},
  {"number":23,"headRefName":"bot/client-happ-windows-desktop-v4.8.0"},
  {"number":24,"headRefName":"bot/client-happ-android-4.6.0"},
  {"number":25,"headRefName":"bot/client-happ-windows-desktop-v4.9.0"},
  {"number":26,"headRefName":"bot/xray-26.9.9"},
  {"number":27,"headRefName":"bot/xray-26.10.31"},
  {"number":28,"headRefName":"bot/xray-26.11.0"},
  {"number":29,"headRefName":"bot/deps-fix-2026-10-28"}
]
EOF
echo '[]' >"$work/no-prs.json"

check "устаревшие: клиент, старше новой версии, новее и та же версия не закрываются" $'21\tbot/client-happ-windows-desktop-v4.6.1\n22\tbot/client-happ-windows-desktop-v4.7.0' "$(bash "$dir/superseded.sh" desktop-v4.8.0 bot/client-happ-windows- <"$work/prs.json")"
check "устаревшие: версии сравниваются по числам (4.10 новее 4.9)" $'21\tbot/client-happ-windows-desktop-v4.6.1\n22\tbot/client-happ-windows-desktop-v4.7.0\n23\tbot/client-happ-windows-desktop-v4.8.0\n25\tbot/client-happ-windows-desktop-v4.9.0' "$(bash "$dir/superseded.sh" desktop-v4.10.0 bot/client-happ-windows- <"$work/prs.json")"
check "устаревшие: та же версия не закрывается" "21	bot/client-happ-windows-desktop-v4.6.1" "$(bash "$dir/superseded.sh" desktop-v4.7.0 bot/client-happ-windows- <"$work/prs.json")"
check "устаревшие: xray, версии сравниваются по числам" $'26\tbot/xray-26.9.9' "$(bash "$dir/superseded.sh" 26.10.31 bot/xray- <"$work/prs.json")"
check "устаревшие: другой клиент не затрагивается" "24	bot/client-happ-android-4.6.0" "$(bash "$dir/superseded.sh" 4.9.0 bot/client-happ-android- <"$work/prs.json")"
check "устаревшие: открытых PR нет — пусто" "" "$(echo '[]' | bash "$dir/superseded.sh" 1.0.0 bot/xray- )"
check "устаревшие: версия без номера — ошибка" error "$(failed bash "$dir/superseded.sh" ver bot/xray- <"$work/prs.json")"
check "устаревшие: нет префикса — ошибка" error "$(failed bash "$dir/superseded.sh" 1.0.0 <"$work/no-prs.json")"

if [ "$failures" -ne 0 ]; then
  echo "провалено проверок: $failures"
  exit 1
fi
echo "все проверки пройдены"
