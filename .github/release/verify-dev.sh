#!/usr/bin/env bash
# Проверка скачанных ресурсов dev-выпуска: набор файлов, SHA256SUMS и аттестации.
#   REPO=<владелец/репозиторий> [SKIP_ATTESTATION=1] verify-dev.sh <каталог> <версия dev>
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

assets=${1:?не указан каталог ресурсов}
version=${2:?не указана версия dev}

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
      --repo "$repo" --signer-workflow "$repo/.github/workflows/release-dev.yml"
  done
fi
