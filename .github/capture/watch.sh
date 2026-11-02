#!/usr/bin/env bash
# Сравнивает последние стабильные релизы клиентов с теми, по которым сняты образцы
# crates/emulation/captures (поле release), и пишет в $GITHUB_OUTPUT список приложений,
# которые нужно обновить: JSON для matrix. Новых версий нет — пустой список.
#   REPO=raycat-app/raycat FORCE=none GH_TOKEN=... bash .github/capture/watch.sh
# FORCE: none | all | happ-windows | happ-android | incy-android — вместо новой версии
# пересобрать профиль по текущей (проверка стенда: профиль не должен измениться).
set -euo pipefail
cd "$(dirname "$0")/../.."
. .github/capture/releases.sh

force=${FORCE:-none}
output=${GITHUB_OUTPUT:-/dev/stdout}

# ключ, приложение, платформа, репозиторий релизов, файл, по которому релиз узнаётся
# как релиз этого клиента (у INCY приложение для Android едет в релизах настольной версии)
clients=(
  "happ-windows happ windows Happ-proxy/happ-desktop setup-Happ.x64.exe"
  "happ-android happ android Happ-proxy/happ-android Happ.apk"
  "incy-android incy android INCY-DEV/incy-platforms Incy.apk"
)

recorded_release() {
  local file
  for file in crates/emulation/captures/*.toml; do
    if grep -qx "app = \"$1\"" "$file" && grep -qx "platform = \"$2\"" "$file"; then
      sed -n 's/^release = "\(.*\)"$/\1/p' "$file" | head -n 1
      return 0
    fi
  done
}

is_newer() {
  local latest_version current_version
  latest_version=$(version_of "$1")
  current_version=$(version_of "$2")
  [ "$latest_version" != "$current_version" ] && [ "$(printf '%s\n%s\n' "$latest_version" "$current_version" | sort -V | tail -n 1)" = "$latest_version" ]
}

entries=()
for client in "${clients[@]}"; do
  read -r key app platform upstream asset <<<"$client"
  current=$(recorded_release "$app" "$platform")
  latest=$(latest_stable "$upstream" "$asset")

  target=""
  case "$force" in
    all | "$key") target=$current ;;
  esac
  if [ -z "$target" ] && [ -n "$latest" ] && is_newer "$latest" "${current:-0}"; then
    target=$latest
  fi
  if [ -z "$target" ]; then
    echo "$key: новой версии нет (в образцах ${current:-нет}, последняя стабильная ${latest:-нет})"
    continue
  fi
  if ! [[ "$target" =~ ^[0-9A-Za-z._-]+$ ]]; then
    echo "::warning::$key: необычный тег релиза, пропуск"
    continue
  fi
  branch="bot/client-$key-$target"
  if [ "$force" = none ]; then
    decision=$(gh pr list -R "$REPO" --head "$branch" --state open --json number,updatedAt,statusCheckRollup | bash .github/bots/pr.sh "$(date -u +%s)" 43200)
    if [[ "$decision" == wait:* ]]; then
      echo "$key: PR из ветки $branch открыт, пропуск (${decision#wait: })"
      continue
    fi
    [[ "$decision" == "go: открытых PR нет" ]] || echo "$key: открытый PR не блокирует ($decision), обновление поверх него"
  fi
  echo "$key: ${current:-нет} -> $target"
  entries+=("$(jq -cn --arg key "$key" --arg app "$app" --arg platform "$platform" --arg release "$target" '{key: $key, app: $app, platform: $platform, release: $release}')")
done

apps=$(printf '%s\n' "${entries[@]}" | jq -cs '.')
echo "apps=$apps" >> "$output"
