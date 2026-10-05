#!/usr/bin/env bash
# Продвижение образов dev-выпуска в stable: те же digest получают теги latest, X.Y.Z, X.Y
# (и latest-noaes, X.Y.Z-noaes). Перед этим проверяется аттестация образов.
#   REGISTRY=<реестр/имя> REPO=<владелец/репозиторий> DEV_VERSION=<версия dev> VERSION=<X.Y.Z> [DRY_RUN=true] promote-images.sh
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

registry=${REGISTRY:?не задан REGISTRY}
repo=${REPO:?не задан REPO}
dev=${DEV_VERSION:?не задана DEV_VERSION}
version=${VERSION:?не задана VERSION}
minor=${version%.*}

digest_of() {
  echo "sha256:$(docker buildx imagetools inspect --raw "$1" | sha256sum | cut -d' ' -f1)"
}

index=$(digest_of "$registry:$dev")
noaes=$(digest_of "$registry:$dev-noaes")
echo "dev $dev: список $index, noaes $noaes"

for digest in "$index" "$noaes"; do
  gh attestation verify "oci://$registry@$digest" \
    --repo "$repo" --signer-workflow "$repo/.github/workflows/release-dev.yml"
done

if [ "${DRY_RUN:-false}" = true ]; then
  echo "Сухой прогон: $registry@$index получил бы теги latest, $version, $minor; $registry@$noaes — latest-noaes, $version-noaes"
  exit 0
fi

docker buildx imagetools create --tag "$registry:latest" --tag "$registry:$version" --tag "$registry:$minor" "$registry@$index"
docker buildx imagetools create --tag "$registry:latest-noaes" --tag "$registry:$version-noaes" "$registry@$noaes"

for tag in latest "$version" "$minor"; do
  [ "$(digest_of "$registry:$tag")" = "$index" ] || fail "тег $tag указывает не на $index"
done
for tag in latest-noaes "$version-noaes"; do
  [ "$(digest_of "$registry:$tag")" = "$noaes" ] || fail "тег $tag указывает не на $noaes"
done
