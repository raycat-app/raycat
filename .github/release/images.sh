#!/usr/bin/env bash
# Публикация образов dev-выпуска из образов CI без пересборки.
#   REPO=<реестр/имя> VERSION=<версия dev> ROLLING=dev IMAGES=<каталог image-*.tar.gz> OUT=<файл> images.sh
# Тегает: <версия> и <ROLLING> (список amd64+arm64), <версия>-noaes и <ROLLING>-noaes (только arm64).
# Служебные теги <версия>-amd64 и <версия>-arm64 нужны для сборки списка и остаются в реестре.
# В OUT пишутся строки index=, amd64=, arm64=, noaes= с digest.
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

repo=${REPO:?не задан REPO}
version=${VERSION:?не задан VERSION}
rolling=${ROLLING:?не задан ROLLING}
images=${IMAGES:?не задан IMAGES}
out=${OUT:?не задан OUT}

push_image() {
  gunzip -c "$images/$1" | docker load
  docker tag raycat:ci "$repo:$2"
  docker push "$repo:$2"
  docker rmi "$repo:$2" raycat:ci
}

push_image image-amd64.tar.gz "$version-amd64"
push_image image-arm64.tar.gz "$version-arm64"
docker buildx imagetools create --tag "$repo:$version" "$repo:$version-amd64" "$repo:$version-arm64"
index=$(digest_of "$repo:$version")
retag "$repo" "$index" "$rolling"

push_image image-arm64-noaes.tar.gz "$version-noaes"
noaes=$(digest_of "$repo:$version-noaes")
retag "$repo" "$noaes" "$rolling-noaes"

amd64=$(docker buildx imagetools inspect --raw "$repo:$version" \
  | jq -r '[.manifests[] | select(.platform.architecture == "amd64") | .digest] | first // empty')
arm64=$(docker buildx imagetools inspect --raw "$repo:$version" \
  | jq -r '[.manifests[] | select(.platform.architecture == "arm64") | .digest] | first // empty')

[ -n "$amd64" ] || fail "в списке образов нет amd64"
[ -n "$arm64" ] || fail "в списке образов нет arm64"
[ "$(digest_of "$repo:$rolling")" = "$index" ] || fail "тег $rolling не указывает на $version"
[ "$(digest_of "$repo:$rolling-noaes")" = "$noaes" ] || fail "тег $rolling-noaes не указывает на $version-noaes"

{
  echo "index=$index"
  echo "amd64=$amd64"
  echo "arm64=$arm64"
  echo "noaes=$noaes"
} >>"$out"
cat "$out"
