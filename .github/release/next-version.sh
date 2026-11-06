#!/usr/bin/env bash
# Номер следующей stable для <ссылки> (по умолчанию HEAD), без «-dev.N»: прошлая stable и
# характер коммитов после неё. Его вшивает в программу сборка (RAYCAT_RELEASE_VERSION) и его
# же dev-version.sh дополняет номером запуска — номер в программе и в выпуске один.
# Нужна история с тегами (checkout с fetch-depth: 0).
#   next-version.sh [<ссылка>]
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ref=${1:-HEAD}
last_tag=$(bash "$dir/stable-tag.sh" "$ref")
last=${last_tag#v}
bash "$dir/collect.sh" "$last_tag" "$ref" | bash "$dir/version.sh" "${last:-0.0.0}"
