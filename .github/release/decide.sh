#!/usr/bin/env bash
# Выбор dev-сборки для продвижения в stable: собирает данные (теги, выпуски, метки PR и
# issue) и запускает plan.sh. Запускать в клоне с полной историей и тегами.
#   REPO=<владелец/репозиторий> GH_TOKEN=... [VERSION_INPUT=<версия dev>] [IGNORE_WAIT=true] decide.sh
# Результат: plan/plan.json и значения promote, tag, sha, version, last_tag в $GITHUB_OUTPUT.
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=${REPO:?не задан REPO}
out=plan
mkdir -p "$out"

stop=false
if [ "$(gh issue list --repo "$repo" --state open --label 'стоп-релиз' --json number --jq 'length')" -gt 0 ]; then
  stop=true
fi

last_tag=$(bash "$dir/stable-tag.sh" HEAD)
last=${last_tag#v}

gh release list --repo "$repo" --limit 500 --exclude-drafts --json tagName,publishedAt,isPrerelease \
  --jq '.[] | select(.isPrerelease and (.tagName | test("^v[0-9]+\\.[0-9]+\\.[0-9]+-dev\\.[0-9]+$"))) | [.tagName, .publishedAt] | @tsv' \
  | while IFS=$'\t' read -r tag published; do
    sha=$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}") || continue
    jq -cn --arg tag "$tag" --arg sha "$sha" --arg published "$published" \
      '{tag: $tag, sha: $sha, published_at: ($published | fromdateiso8601)}'
  done >"$out/builds.jsonl"

bash "$dir/collect.sh" "$last_tag" HEAD >"$out/commits-raw.jsonl"
fix_title='^fix(\(.*\))?:'
while IFS= read -r line; do
  sha=$(jq -r .sha <<<"$line")
  subject=$(jq -r .subject <<<"$line")
  labels='[]'
  if [[ $subject =~ $fix_title ]]; then
    labels=$(gh api "repos/$repo/commits/$sha/pulls" --jq '[.[] | select(.merged_at != null) | .labels[].name]' </dev/null)
  fi
  jq -c --argjson labels "$labels" '. + {labels: $labels}' <<<"$line"
done <"$out/commits-raw.jsonl" >"$out/commits.jsonl"

args=(--commits "$out/commits.jsonl" --builds "$out/builds.jsonl" --last "${last:-0.0.0}" --now "$(date +%s)")
if [ -n "${VERSION_INPUT:-}" ]; then
  args+=(--mode manual --requested "v${VERSION_INPUT#v}")
else
  args+=(--mode auto)
fi
if [ "${IGNORE_WAIT:-false}" = true ]; then
  args+=(--ignore-wait)
fi
if [ "$stop" = true ]; then
  args+=(--stop)
fi

bash "$dir/plan.sh" "${args[@]}" >"$out/plan.json"
bash "$dir/plan-report.sh" "$out/plan.json" | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"

{
  echo "promote=$(jq -r .promote "$out/plan.json")"
  echo "tag=$(jq -r .tag "$out/plan.json")"
  echo "sha=$(jq -r .sha "$out/plan.json")"
  echo "version=$(jq -r .version "$out/plan.json")"
  echo "last_tag=$last_tag"
} >>"${GITHUB_OUTPUT:-/dev/stdout}"
