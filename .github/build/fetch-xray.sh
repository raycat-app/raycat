#!/usr/bin/env bash
# Скачивает xray для linux/amd64 из .github/versions.toml, проверяет sha256 и кладёт файл xray
# в каталог (по умолчанию текущий). Запускать из корня репозитория.
#   bash .github/build/fetch-xray.sh [каталог]
# С XRAY_CACHE=<каталог> архив берётся оттуда, если уже скачан, и кладётся туда после
# скачивания. Контрольная сумма проверяется всегда, в том числе у архива из кэша.
set -euo pipefail

dest=${1:-.}
version=$(sed -n '/^\[xray\]$/,/^\[/ s/^version = "\(.*\)"/\1/p' .github/versions.toml)
sha256=$(sed -n 's/^x86_64 = "\(.*\)"/\1/p' .github/versions.toml)
test -n "$version" && test -n "$sha256"

archive=Xray-linux-64.zip
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cache=${XRAY_CACHE:-}
cached=${cache:+$cache/$version-$archive}

if [ -n "$cached" ] && [ -f "$cached" ] && echo "${sha256}  $cached" | sha256sum -c --status -; then
  echo "xray $version из кэша"
  cp "$cached" "$tmp/$archive"
else
  curl -fsSL --retry 5 --retry-all-errors -o "$tmp/$archive" "https://github.com/XTLS/Xray-core/releases/download/v${version}/${archive}"
fi
echo "${sha256}  $tmp/$archive" | sha256sum -c -
if [ -n "$cached" ]; then
  mkdir -p "$cache"
  cp "$tmp/$archive" "$cached"
fi
mkdir -p "$dest"
unzip -q -o "$tmp/$archive" xray -d "$dest"
"$dest/xray" version
