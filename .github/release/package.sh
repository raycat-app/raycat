#!/usr/bin/env bash
# Архивы выпуска и SHA256SUMS из подготовленных файлов (inputs.sh).
#   SOURCE_DATE_EPOCH=<секунды> package.sh <версия> <каталог файлов> <каталог результата>
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

version=${1:?не указана версия}
inputs=${2:?не указан каталог файлов}
out=${3:?не указан каталог результата}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$out"

# Архивы сжимаются одновременно: каждый собирается в своём каталоге и пишется в свой файл.
pids=()
for suffix in "${ARCHIVE_SUFFIXES[@]}"; do
  (
  case $suffix in
    x86_64-linux-musl) binary=raycat-x86_64 xray=xray-x86_64 ;;
    aarch64-linux-musl) binary=raycat-aarch64 xray=xray-aarch64 ;;
    aarch64-linux-musl-noaes) binary=raycat-aarch64 xray=xray-aarch64-noaes ;;
    armv7-linux-musleabihf) binary=raycat-armv7 xray=xray-armv7 ;;
    armv7-linux-musleabihf-noaes) binary=raycat-armv7 xray=xray-armv7-noaes ;;
    *) fail "неизвестная цель $suffix" ;;
  esac
  name="raycat-${version}-${suffix}"
  root="$stage/$name"
  install -d -m 755 "$root" "$root/completions" "$root/man" "$root/systemd"
  install -m 755 "$inputs/$binary" "$root/raycat"
  install -m 755 "$inputs/$xray" "$root/xray"
  install -m 644 "$inputs/LICENSE" "$root/LICENSE"
  install -m 644 "$inputs/README.md" "$root/README.md"
  install -m 644 "$inputs/raycat.bash" "$root/completions/raycat.bash"
  install -m 644 "$inputs/_raycat" "$root/completions/_raycat"
  install -m 644 "$inputs/raycat.fish" "$root/completions/raycat.fish"
  install -m 644 "$inputs/raycat.1" "$root/man/raycat.1"
  install -m 644 "$inputs/raycat.service" "$root/systemd/raycat.service"
  pack "$stage" "$name" "$out/$name.tar.gz"
  ) &
  pids+=($!)
done
for pid in "${pids[@]}"; do
  wait "$pid"
done

(cd "$out" && sha256sum raycat-*.tar.gz >SHA256SUMS)
