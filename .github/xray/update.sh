#!/usr/bin/env bash
# Записывает релиз Xray в versions.toml: версию в секции [xray] и sha256 архивов linux (x86_64,
# aarch64, armv7). Секция [xray.noaes] не трогается: её версию и коммит ведут отдельно. Sha256
# каждого архива должен совпасть со строкой SHA2-256 его .dgst. Меняются только значения этих
# ключей, остальной файл остаётся как был. Ошибка до записи не трогает файл.
#   update.sh <версия без v> <каталог с архивами и .dgst> [versions.toml]
set -euo pipefail

version=${1:?не указана версия}
dir=${2:?не указан каталог архивов}
file=${3:-versions.toml}

fail() {
  echo "ОШИБКА: $*" >&2
  exit 1
}

[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "версия не вида X.Y.Z: $version"
[ -f "$file" ] || fail "нет файла $file"
[ "$(sed -n '/^\[xray\]$/,/^\[/p' "$file" | grep -c '^version = "')" -eq 1 ] || fail "в $file в секции [xray] ключ version должен встречаться ровно один раз"
for key in x86_64 aarch64 armv7; do
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

sed -i "/^\[xray\]\$/,/^\[/ s/^version = \".*\"\$/version = \"$version\"/" "$file"
set_value x86_64 "${sums[x86_64]}"
set_value aarch64 "${sums[aarch64]}"
set_value armv7 "${sums[armv7]}"
