#!/usr/bin/env bash
# Удаляет старые dev-выпуски вместе с тегами. Остаются KEEP (по умолчанию 20) новейших
# и все, что ещё не вошло в stable: от них считается срок выдержки изменений.
# Образы в реестре не трогаются. Запускать в клоне с тегами.
#   REPO=<владелец/репозиторий> GH_TOKEN=... cleanup.sh
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=${REPO:?не задан REPO}
keep=${KEEP:-20}
last=$(bash "$dir/stable-tag.sh" HEAD)

old=$(gh release list --repo "$repo" --limit 500 --exclude-drafts --json tagName,publishedAt,isPrerelease \
  | jq -r --argjson keep "$keep" '
      [.[] | select(.isPrerelease and (.tagName | test("^v[0-9]+\\.[0-9]+\\.[0-9]+-dev\\.[0-9]+$")))]
      | sort_by(.publishedAt) | reverse | .[$keep:] | .[].tagName')

while IFS= read -r tag; do
  [ -n "$tag" ] || continue
  if [ -n "$last" ] && git merge-base --is-ancestor "refs/tags/$tag" "refs/tags/$last"; then
    gh release delete "$tag" --repo "$repo" --cleanup-tag --yes
    echo "Удалён $tag"
  else
    echo "Оставлен $tag: ещё не вошёл в stable"
  fi
done <<<"$old"
