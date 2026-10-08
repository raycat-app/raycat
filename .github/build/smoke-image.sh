#!/usr/bin/env bash
# Дымовая проверка собранного образа raycat:ci. Запускать из корня репозитория.
#   ARCH=arm64-noaes XRAY_BUILD=noaes bash .github/build/smoke-image.sh
# ARCH нужен для строки в сводке, XRAY_BUILD=noaes включает проверку версии Go.
set -euo pipefail

image=raycat:ci
xray=/usr/libexec/raycat/xray

version_line=$(docker run --rm "$image" --version)
echo "$version_line"
[[ "$version_line" == *" (${GITHUB_SHA:0:7})" ]]
healthcheck=$(docker image inspect -f '{{json .Config.Healthcheck.Test}}' "$image")
test "$healthcheck" = '["CMD","raycat","health"]'
socket=$(docker run --rm --entrypoint printenv "$image" RAYCAT_SOCKET)
test "$socket" = /var/lib/raycat/raycat.sock
if health_output=$(docker run --rm "$image" health 2>&1); then
  echo "raycat health без демона завершился успешно" >&2
  exit 1
fi
echo "$health_output"
grep -q 'raycat не запущен' <<<"$health_output"
version_output=$(docker run --rm --entrypoint "$xray" "$image" version)
echo "$version_output"
if [ "${XRAY_BUILD:-}" = noaes ]; then
  go_version=$(sed -n 's/^go_version = "\(.*\)"/\1/p' versions.toml)
  grep -qF "(go${go_version} " <<<"$version_output"
fi
# Все конфиги проверяются одним запуском контейнера, а не по контейнеру на файл.
docker run --rm -v "$PWD/crates/xray/tests/golden:/golden:ro" --entrypoint sh "$image" -c '
  status=0
  for config in /golden/*.json; do
    echo "::group::${config}"
    '"$xray"' run -test -c "$config" || status=1
    echo "::endgroup::"
  done
  exit "$status"
'
docker run --rm --entrypoint nft "$image" --version
docker run --rm --entrypoint ip "$image" -V
volumes=$(docker image inspect -f '{{json .Config.Volumes}}' "$image")
test "$volumes" = null
command=$(docker image inspect -f '{{json .Config.Entrypoint}} {{json .Config.Cmd}}' "$image")
test "$command" = '["raycat"] ["daemon"]'
size=$(docker image inspect -f '{{.Size}}' "$image")
echo "Образ ${ARCH:-?}: $((size / 1024 / 1024)) МиБ ($size байт)"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  echo "Образ ${ARCH:-?}: $((size / 1024 / 1024)) МиБ" >>"$GITHUB_STEP_SUMMARY"
fi
