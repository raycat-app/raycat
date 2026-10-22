#!/usr/bin/env bash
# Проверка скачанных ресурсов выпуска (dev или stable): набор файлов, SHA256SUMS и аттестации.
# Аттестация проверяется у того workflow, который выпускает эту версию.
#   REPO=<владелец/репозиторий> [SKIP_ATTESTATION=1] verify-dev.sh <каталог> <версия>
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

assets=${1:?не указан каталог ресурсов}
version=${2:?не указана версия}
case $version in
  *-dev.*) signer=release-dev.yml ;;
  *) signer=release-stable.yml ;;
esac

expected=$({
  echo SHA256SUMS
  for suffix in "${ARCHIVE_SUFFIXES[@]}"; do
    echo "raycat-${version}-${suffix}.tar.gz"
  done
} | LC_ALL=C sort)
actual=$(ls -A "$assets" | LC_ALL=C sort)
[ "$expected" = "$actual" ] || fail "набор файлов dev-выпуска отличается от ожидаемого:
$actual"

(cd "$assets" && sha256sum -c SHA256SUMS)

if [ -z "${SKIP_ATTESTATION:-}" ]; then
  repo=${REPO:?не задан REPO}
  for suffix in "${ARCHIVE_SUFFIXES[@]}"; do
    gh attestation verify "$assets/raycat-${version}-${suffix}.tar.gz" \
      --repo "$repo" --signer-workflow "$repo/.github/workflows/$signer"
  done
fi
