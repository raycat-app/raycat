#!/usr/bin/env bash
# Архивы dev-выпуска под версию stable. Меняются только имена архива и каталога внутри,
# файлы остаются теми же; после упаковки хэши и права файлов сверяются с исходными.
#   SOURCE_DATE_EPOCH=<секунды> repack.sh <версия dev> <версия stable> <каталог dev-архивов> <каталог результата>
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

dev=${1:?не указана версия dev}
stable=${2:?не указана версия stable}
src=${3:?не указан каталог dev-архивов}
out=${4:?не указан каталог результата}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

for suffix in "${ARCHIVE_SUFFIXES[@]}"; do
  old="raycat-${dev}-${suffix}"
  new="raycat-${stable}-${suffix}"
  [ -f "$src/$old.tar.gz" ] || fail "нет архива $old.tar.gz"

  rm -rf "$work/old" "$work/new" "$work/check"
  mkdir "$work/old" "$work/new" "$work/check"
  tar -xzpf "$src/$old.tar.gz" -C "$work/old"
  [ "$(find "$work/old" -mindepth 1 -maxdepth 1 | wc -l)" -eq 1 ] || fail "в $old.tar.gz не один каталог верхнего уровня"
  [ -d "$work/old/$old" ] || fail "в $old.tar.gz нет каталога $old"

  before=$(fingerprint "$work/old/$old")
  mv "$work/old/$old" "$work/new/$new"
  pack "$work/new" "$new" "$out/$new.tar.gz"
  tar -xzpf "$out/$new.tar.gz" -C "$work/check"
  after=$(fingerprint "$work/check/$new")
  [ "$before" = "$after" ] || fail "содержимое $new.tar.gz отличается от $old.tar.gz"
done

(cd "$out" && sha256sum raycat-*.tar.gz >SHA256SUMS)
