#!/usr/bin/env bash
# Тесты разбора отчёта cargo-deny на выдуманных фикстурах в формате --format json: выбор
# пакетов, заголовок PR и отчёт для описания. Запуск: bash .github/deps/test.sh
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

contains() {
  case $1 in
    *"$2"*) echo yes ;;
    *) echo no ;;
  esac
}

pick() {
  bash "$dir/select.sh" "$1"
}

rep() {
  bash "$dir/report.sh" "$1"
}

close() {
  bash "$dir/close.sh" "$1"
}

# Фикстуры: выдуманные крейты alpha, beta, gamma, delta и идентификаторы RUSTSEC-2099-…

cat >"$work/mixed.jsonl" <<'EOF'
{"type":"log","fields":{"timestamp":"2099-01-01T00:00:00Z","level":"WARN","message":"фикстура"}}
не JSON
{"type":"diagnostic","fields":{"severity":"error","code":"vulnerability","message":"Fixture vulnerability","graphs":[{"Krate":{"name":"alpha","version":"1.2.3"},"parents":[{"Krate":{"name":"app","version":"0.1.0"}}]}],"advisory":{"id":"RUSTSEC-2099-0001","package":"alpha"}}}
{"type":"diagnostic","fields":{"severity":"error","code":"yanked","message":"detected yanked crate (try `cargo update -p beta`)","graphs":[{"Krate":{"name":"beta","version":"0.4.0"},"parents":[]}]}}
{"type":"diagnostic","fields":{"severity":"error","code":"unmaintained","message":"Fixture unmaintained","graphs":[{"Krate":{"name":"gamma","version":"2.0.0"}}],"advisory":{"id":"RUSTSEC-2099-0002","package":"gamma"}}}
{"type":"diagnostic","fields":{"severity":"warning","code":"vulnerability","message":"Fixture warning","graphs":[{"Krate":{"name":"delta","version":"3.0.0"}}],"advisory":{"id":"RUSTSEC-2099-0003","package":"delta"}}}
EOF

cat >"$work/yanked.jsonl" <<'EOF'
{"type":"diagnostic","fields":{"severity":"error","code":"yanked","message":"detected yanked crate (try `cargo update -p beta`)","graphs":[{"Krate":{"name":"beta","version":"0.4.0"},"parents":[]}]}}
EOF

cat >"$work/other.jsonl" <<'EOF'
{"type":"diagnostic","fields":{"severity":"error","code":"unmaintained","message":"Fixture unmaintained","graphs":[{"Krate":{"name":"gamma","version":"2.0.0"}}],"advisory":{"id":"RUSTSEC-2099-0002","package":"gamma"}}}
{"type":"diagnostic","fields":{"severity":"warning","code":"vulnerability","message":"Fixture warning","graphs":[{"Krate":{"name":"delta","version":"3.0.0"}}],"advisory":{"id":"RUSTSEC-2099-0003","package":"delta"}}}
EOF

cat >"$work/warn.jsonl" <<'EOF'
{"type":"diagnostic","fields":{"severity":"warning","code":"vulnerability","message":"Fixture warning","graphs":[{"Krate":{"name":"delta","version":"3.0.0"}}],"advisory":{"id":"RUSTSEC-2099-0003","package":"delta"}}}
EOF

cat >"$work/nograph.jsonl" <<'EOF'
{"type":"diagnostic","fields":{"severity":"error","code":"vulnerability","message":"Fixture fallback","advisory":{"id":"RUSTSEC-2099-0004","package":"delta"}}}
EOF

cat >"$work/dupes.jsonl" <<'EOF'
{"type":"diagnostic","fields":{"severity":"error","code":"vulnerability","message":"a","graphs":[{"Krate":{"name":"alpha","version":"1.2.3"}}],"advisory":{"id":"RUSTSEC-2099-0006","package":"alpha"}}}
{"type":"diagnostic","fields":{"severity":"error","code":"vulnerability","message":"b","graphs":[{"Krate":{"name":"alpha","version":"1.2.3"}}],"advisory":{"id":"RUSTSEC-2099-0005","package":"alpha"}}}
{"type":"diagnostic","fields":{"severity":"error","code":"vulnerability","message":"c","graphs":[{"Krate":{"name":"alpha","version":"1.2.3"}}],"advisory":{"id":"RUSTSEC-2099-0005","package":"alpha"}}}
EOF

check "выбор: уязвимость и отозванная версия, прочее отброшено" \
  '{"specs":["alpha@1.2.3","beta@0.4.0"],"advisories":["RUSTSEC-2099-0001"],"yanked":["beta"],"title":"fix: обновление уязвимых зависимостей (RUSTSEC-2099-0001; отозванные версии: beta)"}' \
  "$(pick "$work/mixed.jsonl")"
check "выбор: только отозванная версия" \
  '{"specs":["beta@0.4.0"],"advisories":[],"yanked":["beta"],"title":"fix: обновление уязвимых зависимостей (отозванные версии: beta)"}' \
  "$(pick "$work/yanked.jsonl")"
check "выбор: прочие ошибки и предупреждения не исправляются" \
  '{"specs":[],"advisories":[],"yanked":[],"title":""}' \
  "$(pick "$work/other.jsonl")"
check "выбор: без графа пакет берётся из advisory" \
  '{"specs":["delta"],"advisories":["RUSTSEC-2099-0004"],"yanked":[],"title":"fix: обновление уязвимых зависимостей (RUSTSEC-2099-0004)"}' \
  "$(pick "$work/nograph.jsonl")"
check "выбор: повторы убираются, идентификаторы по порядку" \
  '{"specs":["alpha@1.2.3"],"advisories":["RUSTSEC-2099-0005","RUSTSEC-2099-0006"],"yanked":[],"title":"fix: обновление уязвимых зависимостей (RUSTSEC-2099-0005, RUSTSEC-2099-0006)"}' \
  "$(pick "$work/dupes.jsonl")"

if bash "$dir/select.sh" "$work/missing.jsonl" >/dev/null 2>&1; then
  check "выбор: нет файла отчёта — ошибка" error ok
else
  check "выбор: нет файла отчёта — ошибка" error error
fi

expected=$(
  cat <<'EOF'
- `vulnerability`: Fixture vulnerability; крейт: alpha 1.2.3 (RUSTSEC-2099-0001)
- `yanked`: detected yanked crate (try `cargo update -p beta`); крейт: beta 0.4.0
- `unmaintained`: Fixture unmaintained; крейт: gamma 2.0.0 (RUSTSEC-2099-0002)
EOF
)
check "отчёт: все ошибки уровня error с крейтами" "$expected" "$(rep "$work/mixed.jsonl")"
check "отчёт: предупреждения не попадают" "- ошибок уровня error в отчёте нет" "$(rep "$work/warn.jsonl")"
check "отчёт: идентификатор RustSec назван" yes "$(contains "$(rep "$work/nograph.jsonl")" "(RUSTSEC-2099-0004)")"

cat >"$work/issues.json" <<'EOF'
[
  {"number":21,"title":"Зависимости: не удалось исправить автоматически"},
  {"number":22,"title":"Зависимости: что-то другое"},
  {"number":23,"title":"Зависимости: PR бота не прошёл проверки"}
]
EOF

echo '[]' >"$work/no-issues.json"

check "закрытие: только issue бота по заголовку" $'21\n23' "$(close "$work/issues.json")"
check "закрытие: открытых issue бота нет" "" "$(close "$work/no-issues.json")"

if bash "$dir/close.sh" "$work/missing.json" >/dev/null 2>&1; then
  check "закрытие: нет файла со списком — ошибка" error ok
else
  check "закрытие: нет файла со списком — ошибка" error error
fi

if [ "$failures" -gt 0 ]; then
  echo "провалено проверок: $failures"
  exit 1
fi
echo "все проверки пройдены"
