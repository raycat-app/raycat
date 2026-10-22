#!/usr/bin/env bash
# Образ выпуска с новым xray для одного варианта: тот же raycat, что в архивах выпуска
# (dist/<архитектура>/raycat), и xray по versions.toml тем же Dockerfile, что в CI. Образ проходит
# дымовую проверку и сохраняется как image-<вариант>.tar.gz для images.sh. Дымовая проверка сверяет
# коммит raycat с SMOKE_COMMIT (коммит прошлой stable). Собирать на раннере той же архитектуры:
# дымовая проверка nft не работает под эмуляцией.
# Запускать из корня репозитория: нужны docker с buildx.
#   SMOKE_COMMIT=<полный sha> xray-images.sh <версия> <каталог архивов выпуска> <каталог результата> <amd64|arm64|arm64-noaes>
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

version=${1:?не указана версия}
archives=${2:?не указан каталог архивов выпуска}
out=${3:?не указан каталог результата}
variant=${4:?не указан вариант образа}
: "${SMOKE_COMMIT:?не задан SMOKE_COMMIT}"
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

case $variant in
  amd64)
    platform="linux/amd64"
    suffix="x86_64-linux-musl"
    dist="amd64"
    key="x86_64"
    archive="Xray-linux-64.zip"
    build="release"
    ;;
  arm64)
    platform="linux/arm64"
    suffix="aarch64-linux-musl"
    dist="arm64"
    key="aarch64"
    archive="Xray-linux-arm64-v8a.zip"
    build="release"
    ;;
  arm64-noaes)
    platform="linux/arm64"
    suffix="aarch64-linux-musl"
    dist="arm64"
    key="aarch64"
    archive="Xray-linux-arm64-v8a.zip"
    build="noaes"
    ;;
  *) fail "неизвестный вариант образа: $variant" ;;
esac

tar -xzf "$archives/raycat-$version-$suffix.tar.gz" -C "$work"
mkdir -p "dist/$dist"
install -m 755 "$work/raycat-$version-$suffix/raycat" "dist/$dist/raycat"

bash .github/build/docker-args.sh "$key" "$archive" "$build" >"$work/args"
mapfile -t args <"$work/args"
docker buildx build --platform "$platform" --load "${args[@]}" --tag raycat:ci .
ARCH=$variant XRAY_BUILD=$build bash .github/build/smoke-image.sh
docker save raycat:ci | gzip -1 >"$out/image-$variant.tar.gz"
