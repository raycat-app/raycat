#!/usr/bin/env bash
# Коммиты main в диапазоне <база>..<конец> от старых к новым, по строке JSON на коммит:
# {sha, subject, breaking, files}. Пустая база — вся история.
#   collect.sh [<база>] <конец>
set -euo pipefail

base=${1-}
head=${2:?не указан конец диапазона}
if [ -n "$base" ]; then
  range="$base..$head"
else
  range=$head
fi

git rev-list --reverse --first-parent "$range" | while read -r sha; do
  subject=$(git show -s --format=%s "$sha")
  body=$(git show -s --format=%b "$sha")
  breaking=false
  if grep -Eq '^BREAKING[ -]CHANGE:' <<<"$body"; then
    breaking=true
  fi
  files=$(git diff-tree --no-commit-id --name-only -r --root "$sha" | jq -R . | jq -cs .)
  jq -cn --arg sha "$sha" --arg subject "$subject" --argjson breaking "$breaking" --argjson files "$files" \
    '{sha: $sha, subject: $subject, breaking: $breaking, files: $files}'
done
