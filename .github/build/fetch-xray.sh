#!/usr/bin/env bash
# Скачивает xray для linux/amd64 из versions.toml, проверяет sha256 и кладёт файл xray
# в каталог (по умолчанию текущий). Запускать из корня репозитория.
#   bash .github/build/fetch-xray.sh [каталог]
set -euo pipefail

dest=${1:-.}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' versions.toml)
sha256=$(sed -n 's/^x86_64 = "\(.*\)"/\1/p' versions.toml)
test -n "$version" && test -n "$sha256"

archive=Xray-linux-64.zip
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/$archive" "https://github.com/XTLS/Xray-core/releases/download/v${version}/${archive}"
echo "${sha256}  $tmp/$archive" | sha256sum -c -
mkdir -p "$dest"
unzip -q -o "$tmp/$archive" xray -d "$dest"
"$dest/xray" version
