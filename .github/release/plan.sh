#!/usr/bin/env bash
# Решение о продвижении dev-сборки в stable: JSON в stdout, логика в plan.jq.
#   plan.sh --commits <файл> --builds <файл> --now <секунды> [--last X.Y.Z]
#           [--mode auto|manual] [--requested vX.Y.Z-dev.N] [--ignore-wait] [--stop]
# Файлы — JSON Lines: коммиты (collect.sh, с полем labels) и dev-выпуски {tag, sha, published_at}.
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
commits=
builds=
now=
last=0.0.0
mode=auto
requested=
ignore_wait=false
stop=false

while [ $# -gt 0 ]; do
  case $1 in
    --commits) commits=${2:?не указан файл коммитов}; shift 2 ;;
    --builds) builds=${2:?не указан файл сборок}; shift 2 ;;
    --now) now=${2:?не указано время}; shift 2 ;;
    --last) last=${2:?не указана версия}; shift 2 ;;
    --mode) mode=${2:?не указан режим}; shift 2 ;;
    --requested) requested=${2:?не указана сборка}; shift 2 ;;
    --ignore-wait) ignore_wait=true; shift ;;
    --stop) stop=true; shift ;;
    *) echo "ОШИБКА: неизвестный параметр $1" >&2; exit 2 ;;
  esac
done

fail() {
  echo "ОШИБКА: $*" >&2
  exit 2
}

[ -f "$commits" ] || fail "нет файла коммитов"
[ -f "$builds" ] || fail "нет файла сборок"
[[ $now =~ ^[0-9]+$ ]] || fail "время должно быть числом секунд"
[[ $last =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "версия прошлой stable не вида X.Y.Z"
case $mode in
  auto) ;;
  manual) [[ $requested =~ ^v[0-9]+\.[0-9]+\.[0-9]+-dev\.[0-9]+$ ]] || fail "для ручного режима нужна сборка vX.Y.Z-dev.N" ;;
  *) fail "режим: auto или manual" ;;
esac

jq -n -L "$dir" \
  --slurpfile commits "$commits" \
  --slurpfile builds "$builds" \
  --argjson now "$now" \
  --arg last "$last" \
  --arg mode "$mode" \
  --arg requested "$requested" \
  --argjson ignore_wait "$ignore_wait" \
  --argjson stop "$stop" \
  -f "$dir/plan.jq"
