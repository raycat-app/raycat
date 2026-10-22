#!/usr/bin/env bash
# Образы выпуска с новым xray: тот же raycat, что в архивах выпуска (dist/<архитектура>/raycat),
# и xray по versions.toml тем же Dockerfile, что в CI. Каждый образ проходит дымовую проверку;
# в каталоге результата появляются image-amd64.tar.gz, image-arm64.tar.gz и image-arm64-noaes.tar.gz
# для images.sh. Дымовая проверка сверяет коммит raycat с SMOKE_COMMIT (коммит прошлой stable).
# Запускать из корня репозитория: нужны docker с buildx и binfmt для arm64 (см. workflow).
#   SMOKE_COMMIT=<полный sha> xray-images.sh <версия> <каталог архивов выпуска> <каталог результата>
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

version=${1:?не указана версия}
archives=${2:?не указан каталог архивов выпуска}
out=${3:?не указан каталог результата}
: "${SMOKE_COMMIT:?не задан SMOKE_COMMIT}"
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# raycat_from <суффикс архива> <каталог dist>: бинарник из архива выпуска в контекст сборки.
raycat_from() {
  tar -xzf "$archives/raycat-$version-$1.tar.gz" -C "$work"
  mkdir -p "$2"
  install -m 755 "$work/raycat-$version-$1/raycat" "$2/raycat"
  rm -rf "${work:?}/raycat-$version-$1"
}

# image <имя> <платформа> <ключ xray> <архив xray> <сборка xray>
image() {
  local name=$1 platform=$2 key=$3 archive=$4 build=$5
  local -a args
  bash .github/build/docker-args.sh "$key" "$archive" "$build" >"$work/args"
  mapfile -t args <"$work/args"
  docker buildx build --platform "$platform" --load "${args[@]}" --tag raycat:ci .
  ARCH=$name XRAY_BUILD=$build bash .github/build/smoke-image.sh
  docker save raycat:ci | gzip -1 >"$out/image-$name.tar.gz"
  docker image rm raycat:ci >/dev/null
}

raycat_from x86_64-linux-musl dist/amd64
image amd64 linux/amd64 x86_64 Xray-linux-64.zip release

raycat_from aarch64-linux-musl dist/arm64
image arm64 linux/arm64 aarch64 Xray-linux-arm64-v8a.zip release
image arm64-noaes linux/arm64 aarch64 Xray-linux-arm64-v8a.zip noaes
