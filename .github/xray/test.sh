#!/usr/bin/env bash
# Тесты обновления xray на выдуманных данных, без сети: выбор версии по списку релизов и
# запись versions.toml. Запуск: bash .github/xray/test.sh
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

# Выбор версии

full="Xray-linux-64.zip Xray-linux-64.zip.dgst Xray-linux-arm64-v8a.zip Xray-linux-arm64-v8a.zip.dgst Xray-linux-arm32-v7a.zip Xray-linux-arm32-v7a.zip.dgst"
no_dgst="Xray-linux-64.zip Xray-linux-64.zip.dgst Xray-linux-arm64-v8a.zip Xray-linux-arm64-v8a.zip.dgst Xray-linux-arm32-v7a.zip"
partial="Xray-linux-64.zip Xray-linux-64.zip.dgst"

release() {
  jq -cn --arg tag "$1" --arg at "$2" --arg files "$3" --argjson draft "$4" \
    '{tag_name: $tag, draft: $draft, published_at: $at, assets: ($files | split(" ") | map({name: .}))}'
}

{
  release v1.2.5 2030-01-01T00:00:00Z "$full" false
  release v1.2.0 2030-02-01T00:00:00Z "$full" false
  release v1.3.0 2030-03-01T00:00:00Z "$full" false
  release v1.9.0 2030-04-01T00:00:00Z "$full" false
  release v1.10.0 2030-05-01T00:00:00Z "$full" false
  release v1.5.0 2030-06-01T00:00:00Z "$full" true
  release v1.6.0-rc.1 2030-07-01T00:00:00Z "$full" false
  release v1.7.0 2030-08-01T00:00:00Z "$partial" false
  release v1.7.1 2030-09-01T00:00:00Z "$no_dgst" false
} | jq -s . >"$work/releases.json"
echo '[]' >"$work/empty.json"

sel() {
  bash "$dir/select.sh" "$work/releases.json" "$@"
}

check "выбор: самый новый готовый релиз по дате публикации" 1.10.0 "$(sel 1.2.0)"
check "выбор: версии сравниваются по числам (1.10 новее 1.9)" 1.10.0 "$(sel 1.9.9)"
check "выбор: текущая актуальна — ничего не печатает" "" "$(sel 1.10.0)"
check "выбор: явная версия с префиксом v" 1.3.0 "$(sel 1.2.0 v1.3.0)"
check "выбор: явная версия новее текущей" 1.2.5 "$(sel 1.2.0 1.2.5)"
check "выбор: явная версия, равная текущей — ничего" "" "$(sel 1.10.0 1.10.0)"
check "выбор: явная версия старее текущей — ошибка" error "$(failed sel 1.10.0 1.9.0)"
check "выбор: черновик не выбирается — ошибка" error "$(failed sel 1.2.0 1.5.0)"
check "выбор: тег с суффиксом не выбирается — ошибка" error "$(failed sel 1.2.0 1.6.0)"
check "выбор: неполный релиз не выбирается — ошибка" error "$(failed sel 1.2.0 1.7.0)"
check "выбор: нет .dgst — ошибка" error "$(failed sel 1.2.0 1.7.1)"
check "выбор: нет такого релиза — ошибка" error "$(failed sel 1.2.0 9.9.9)"
check "выбор: текущая версия не вида X.Y.Z — ошибка" error "$(failed bash "$dir/select.sh" "$work/releases.json" abc)"
check "выбор: нет готовых релизов — ошибка" error "$(failed bash "$dir/select.sh" "$work/empty.json" 1.0.0)"

# Запись versions.toml

dl="$work/dl"
mkdir -p "$dl"
printf 'xray for amd64\n' >"$dl/Xray-linux-64.zip"
printf 'xray for arm64\n' >"$dl/Xray-linux-arm64-v8a.zip"
printf 'xray for armv7\n' >"$dl/Xray-linux-arm32-v7a.zip"
for archive in Xray-linux-64.zip Xray-linux-arm64-v8a.zip Xray-linux-arm32-v7a.zip; do
  printf 'MD5= 00000000000000000000000000000000\nSHA2-256= %s\nSHA2-512= 00\n' \
    "$(sha256sum "$dl/$archive" | cut -d' ' -f1)" >"$dl/$archive.dgst"
done
sum64=$(sha256sum "$dl/Xray-linux-64.zip" | cut -d' ' -f1)
sum_arm64=$(sha256sum "$dl/Xray-linux-arm64-v8a.zip" | cut -d' ' -f1)
sum_arm32=$(sha256sum "$dl/Xray-linux-arm32-v7a.zip" | cut -d' ' -f1)
commit=0123456789abcdef0123456789abcdef01234567

zeros=0000000000000000000000000000000000000000000000000000000000000000
cat >"$work/versions.toml" <<TOML
[xray]
version = "1.2.0"

[xray.sha256]
x86_64 = "$zeros"
aarch64 = "$zeros"
armv7 = "$zeros"

# Сборка xray для процессоров без аппаратного AES.
[xray.noaes]
commit = "ffffffffffffffffffffffffffffffffffffffff"
go_version = "1.26.8"
go_image = "golang:1.26.8-alpine@sha256:$zeros"
upstream_go = "1.26"
TOML

printf 'module example.com/xray\n\ngo 1.27\n\nrequire (\n)\n' >"$work/go.mod"

cat >"$work/expected.toml" <<TOML
[xray]
version = "1.3.0"

[xray.sha256]
x86_64 = "$sum64"
aarch64 = "$sum_arm64"
armv7 = "$sum_arm32"

# Сборка xray для процессоров без аппаратного AES.
[xray.noaes]
commit = "$commit"
go_version = "1.26.8"
go_image = "golang:1.26.8-alpine@sha256:$zeros"
upstream_go = "1.27"
TOML

cp "$work/versions.toml" "$work/orig.toml"
bash "$dir/update.sh" 1.3.0 "$dl" "$commit" "$work/go.mod" "$work/versions.toml"
check "запись: меняются только значения, остальной файл как был" "$(cat "$work/expected.toml")" "$(cat "$work/versions.toml")"

update() {
  cp "$work/orig.toml" "$work/try.toml"
  bash "$dir/update.sh" "$@" "$work/try.toml"
}

check "запись: несовпавший sha256 — ошибка и файл не меняется" error "$(
  rm -rf "$work/bad" && cp -r "$dl" "$work/bad"
  printf 'SHA2-256= %s\n' "$zeros" >"$work/bad/Xray-linux-64.zip.dgst"
  failed update 1.3.0 "$work/bad" "$commit" "$work/go.mod"
)"

check "запись: .dgst без строки SHA2-256 — ошибка" error "$(
  rm -rf "$work/bad" && cp -r "$dl" "$work/bad"
  printf 'MD5= 00\n' >"$work/bad/Xray-linux-arm64-v8a.zip.dgst"
  failed update 1.3.0 "$work/bad" "$commit" "$work/go.mod"
)"

check "запись: нет архива — ошибка" error "$(
  rm -rf "$work/bad" && cp -r "$dl" "$work/bad"
  rm "$work/bad/Xray-linux-arm32-v7a.zip"
  failed update 1.3.0 "$work/bad" "$commit" "$work/go.mod"
)"

check "запись: коммит не SHA — ошибка" error "$(failed update 1.3.0 "$dl" "v26.10.10" "$work/go.mod")"
check "запись: версия не вида X.Y.Z — ошибка" error "$(failed update 1.3 "$dl" "$commit" "$work/go.mod")"

printf 'module example.com/xray\n\nrequire (\n)\n' >"$work/go-nodirective.mod"
check "запись: в go.mod нет строки go — ошибка" error "$(failed update 1.3.0 "$dl" "$commit" "$work/go-nodirective.mod")"

sed '/^commit = /d' "$work/orig.toml" >"$work/nocommit.toml"
cp "$work/nocommit.toml" "$work/nocommit.orig"
check "запись: нет ключа commit — ошибка" error "$(failed bash "$dir/update.sh" 1.3.0 "$dl" "$commit" "$work/go.mod" "$work/nocommit.toml")"
check "запись: при ошибке versions.toml не меняется" "$(cat "$work/nocommit.orig")" "$(cat "$work/nocommit.toml")"

if [ "$failures" -ne 0 ]; then
  echo "Провалено проверок: $failures"
  exit 1
fi
echo "Все проверки пройдены"
