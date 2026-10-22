#!/usr/bin/env bash
# Архивы выпуска с новым xray: файлы raycat те же, что в прошлой stable, меняется только xray.
# Перед упаковкой проверяется, что все остальные файлы и права совпали; после упаковки архив
# распаковывается и сверяется ещё раз.
#   SOURCE_DATE_EPOCH=<секунды> xray-repack.sh <прошлая версия> <новая версия> <каталог архивов прошлой> <каталог файлов xray> <каталог результата>
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

prev=${1:?не указана прошлая версия}
version=${2:?не указана новая версия}
src=${3:?не указан каталог архивов прошлой stable}
xrays=${4:?не указан каталог файлов xray}
out=${5:?не указан каталог результата}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

for suffix in "${ARCHIVE_SUFFIXES[@]}"; do
  case $suffix in
    x86_64-linux-musl) xray=xray-x86_64 ;;
    aarch64-linux-musl) xray=xray-aarch64 ;;
    aarch64-linux-musl-noaes) xray=xray-aarch64-noaes ;;
    armv7-linux-musleabihf) xray=xray-armv7 ;;
    armv7-linux-musleabihf-noaes) xray=xray-armv7-noaes ;;
    *) fail "неизвестная цель $suffix" ;;
  esac
  old="raycat-${prev}-${suffix}"
  new="raycat-${version}-${suffix}"
  [ -f "$src/$old.tar.gz" ] || fail "нет архива $old.tar.gz"
  [ -f "$xrays/$xray" ] || fail "нет файла $xray"

  rm -rf "$work/old" "$work/new" "$work/check"
  mkdir "$work/old" "$work/new" "$work/check"
  tar -xzpf "$src/$old.tar.gz" -C "$work/old"
  [ -d "$work/old/$old" ] || fail "в $old.tar.gz нет каталога $old"

  before=$(fingerprint "$work/old/$old" | grep -v '\./xray$' || true)
  mv "$work/old/$old" "$work/new/$new"
  install -m 755 "$xrays/$xray" "$work/new/$new/xray"
  after=$(fingerprint "$work/new/$new" | grep -v '\./xray$' || true)
  [ "$before" = "$after" ] || fail "в $new изменились файлы кроме xray"

  pack "$work/new" "$new" "$out/$new.tar.gz"
  tar -xzpf "$out/$new.tar.gz" -C "$work/check"
  [ "$(fingerprint "$work/check/$new")" = "$(fingerprint "$work/new/$new")" ] || fail "$new.tar.gz распаковывается не в те же файлы"
done

(cd "$out" && sha256sum raycat-*.tar.gz >SHA256SUMS)
