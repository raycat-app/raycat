#!/usr/bin/env bash
# Печатает аргументы `docker build` из versions.toml: по строке на слово, чтобы читать
# через mapfile. Запускать из корня репозитория.
#
#   bash .github/build/docker-args.sh <ключ архитектуры> <архив xray> [release|noaes]
#   mapfile -t args < <(bash .github/build/docker-args.sh aarch64 Xray-linux-arm64-v8a.zip noaes)
#   docker build "${args[@]}" --tag raycat:ci .
#
# Для noaes проверяет, что закреплённый образ golang — индекс с несколькими платформами.
set -euo pipefail

key=${1:?не указан ключ архитектуры в [xray.sha256]}
archive=${2:?не указан архив xray}
build=${3:-release}

fail() {
  echo "ОШИБКА: $*" >&2
  exit 1
}

value() {
  local found
  found=$(sed -n "s/^$1 = \"\(.*\)\"/\1/p" versions.toml)
  if [ -z "$found" ] || [ "$(printf '%s\n' "$found" | wc -l)" -ne 1 ]; then
    fail "в versions.toml нет ровно одного ключа $1"
  fi
  printf '%s\n' "$found"
}

arg() {
  printf '%s\n%s=%s\n' --build-arg "$1" "$2"
}

arg XRAY_VERSION "$(value version)"
arg XRAY_ARCHIVE "$archive"
arg XRAY_SHA256 "$(value "$key")"
arg XRAY_BUILD "$build"

case "$build" in
  release) ;;
  noaes)
    go_image=$(value go_image)
    digest=${go_image#*@}
    docker buildx imagetools inspect --raw "$go_image" >"${RUNNER_TEMP:-/tmp}/go-index.json"
    index="${RUNNER_TEMP:-/tmp}/go-index.json"
    [ "sha256:$(sha256sum "$index" | cut -d' ' -f1)" = "$digest" ] || fail "digest образа golang не совпал с закреплённым"
    [ "$(jq '.manifests | length' "$index")" -gt 1 ] || fail "закреплён манифест одной платформы, нужен индекс"
    arg XRAY_COMMIT "$(value commit)"
    arg GO_VERSION "$(value go_version)"
    arg GO_IMAGE "$go_image"
    arg XRAY_GO_DIRECTIVE "$(value upstream_go)"
    ;;
  *) fail "неизвестная сборка xray: $build" ;;
esac
