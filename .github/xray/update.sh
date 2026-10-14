#!/usr/bin/env bash
# Записывает релиз Xray в versions.toml: версию, sha256 архивов linux (x86_64, aarch64, armv7),
# коммит тега для сборки noaes и директиву go из go.mod upstream. Sha256 каждого архива должен
# совпасть со строкой SHA2-256 его .dgst. Меняются только значения этих ключей, остальной файл
# остаётся как был. Ошибка до записи не трогает файл.
#   update.sh <версия без v> <каталог с архивами и .dgst> <коммит тега> <go.mod> [versions.toml]
set -euo pipefail

version=${1:?не указана версия}
dir=${2:?не указан каталог архивов}
commit=${3:?не указан коммит тега}
gomod=${4:?не указан go.mod}
file=${5:-versions.toml}

fail() {
  echo "ОШИБКА: $*" >&2
  exit 1
}

[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "версия не вида X.Y.Z: $version"
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "коммит не SHA: $commit"
[ -f "$file" ] || fail "нет файла $file"
[ -f "$gomod" ] || fail "нет файла $gomod"

go=$(sed -n 's/^go \([0-9][0-9.]*\)[[:space:]]*$/\1/p' "$gomod")
[[ $go =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]] || fail "в go.mod нет ровно одной строки go X.Y"

for key in version x86_64 aarch64 armv7 commit upstream_go; do
  [ "$(grep -c "^$key = \"" "$file")" -eq 1 ] || fail "в $file ключ $key должен встречаться ровно один раз"
done

declare -A sums=()
for key in x86_64 aarch64 armv7; do
  case $key in
    x86_64) archive=Xray-linux-64.zip ;;
    aarch64) archive=Xray-linux-arm64-v8a.zip ;;
    armv7) archive=Xray-linux-arm32-v7a.zip ;;
  esac
  [ -f "$dir/$archive" ] || fail "нет архива $archive"
  [ -f "$dir/$archive.dgst" ] || fail "нет файла $archive.dgst"
  expected=$(sed -n 's/^SHA2-256[[:space:]]*=[[:space:]]*\([0-9a-fA-F]\{64\}\)[[:space:]]*$/\1/p' "$dir/$archive.dgst" | tr 'A-F' 'a-f')
  [[ $expected =~ ^[0-9a-f]{64}$ ]] || fail "в $archive.dgst нет ровно одной строки SHA2-256"
  actual=$(sha256sum "$dir/$archive" | cut -d' ' -f1)
  [ "$actual" = "$expected" ] || fail "sha256 $archive не совпал с $archive.dgst"
  sums[$key]=$actual
done

set_value() {
  sed -i "s/^$1 = \".*\"\$/$1 = \"$2\"/" "$file"
}

set_value version "$version"
set_value x86_64 "${sums[x86_64]}"
set_value aarch64 "${sums[aarch64]}"
set_value armv7 "${sums[armv7]}"
set_value commit "$commit"
set_value upstream_go "$go"
