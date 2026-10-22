#!/usr/bin/env bash
# Файлы xray для выпуска с новым xray: обычные сборки — официальные архивы XTLS по versions.toml
# (sha256 проверяется), сборки -noaes для aarch64 и armv7 — той же стадией Dockerfile, что в CI.
# Запускать из корня репозитория: нужны curl, unzip и docker с buildx.
#   xray-files.sh <каталог результата>
# В каталоге появятся xray-x86_64, xray-aarch64, xray-armv7, xray-aarch64-noaes, xray-armv7-noaes.
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

out=${1:?не указан каталог результата}
mkdir -p "$out"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

version=$(sed -n '/^\[xray\]$/,/^\[/ s/^version = "\(.*\)"/\1/p' versions.toml)
[ -n "$version" ] || fail "в versions.toml нет версии xray"

for pair in x86_64:Xray-linux-64.zip aarch64:Xray-linux-arm64-v8a.zip armv7:Xray-linux-arm32-v7a.zip; do
  key=${pair%%:*}
  archive=${pair#*:}
  sha256=$(sed -n "s/^$key = \"\(.*\)\"/\1/p" versions.toml)
  [ -n "$sha256" ] || fail "в versions.toml нет суммы для $key"
  curl -fsSL -o "$tmp/$archive" "https://github.com/XTLS/Xray-core/releases/download/v${version}/${archive}"
  echo "$sha256  $tmp/$archive" | sha256sum -c -
  unzip -q -o "$tmp/$archive" xray -d "$tmp/$key"
  install -m 755 "$tmp/$key/xray" "$out/xray-$key"
done

noaes() {
  local key=$1 archive=$2 platform=$3 dest=$tmp/noaes-$1 pattern description
  bash "$dir/../build/docker-args.sh" "$key" "$archive" noaes >"$tmp/args-$key"
  local -a args
  mapfile -t args <"$tmp/args-$key"
  docker buildx build --platform "$platform" --target xray-noaes --output "type=local,dest=$dest" "${args[@]}" .
  case $key in
    aarch64) pattern='ELF 64-bit.*ARM aarch64' ;;
    armv7) pattern='ELF 32-bit.*ARM, EABI5' ;;
    *) fail "неизвестный ключ $key" ;;
  esac
  description=$(file "$dest/out/xray")
  grep -q "$pattern" <<<"$description" || fail "xray-$key-noaes собран не для нужной платформы"
  install -m 755 "$dest/out/xray" "$out/xray-$key-noaes"
}

noaes aarch64 Xray-linux-arm64-v8a.zip linux/arm64
noaes armv7 Xray-linux-arm32-v7a.zip linux/arm/v7
