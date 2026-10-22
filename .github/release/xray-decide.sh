#!/usr/bin/env bash
# Нужен ли выпуск stable с новым xray и тем же raycat. Он нужен, если последняя stable везёт xray
# старее versions.toml main, а продвинуть новую dev-сборку целиком нельзя (код ещё не выдержал
# срок). Метка «стоп-релиз» этот выпуск отменяет. Версия — следующий патч прошлого тега.
#   xray-decide.sh <xray в main> <xray в прошлой stable или пусто> <прошлый тег или пусто> <стоп: true|false>
# Печатает строки update=, version=, xray=, reason=. Прошлый тег может быть dev-сборкой,
# только если это сухой прогон (тогда workflow передаёт её сам).
set -euo pipefail

main_xray=${1:?не указана версия xray в main}
stable_xray=${2-}
last_tag=${3-}
stop=${4:?не указан признак стоп-релиза}

[[ $main_xray =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "ОШИБКА: версия xray в main не вида X.Y.Z: $main_xray" >&2
  exit 1
}
[[ $stop == true || $stop == false ]] || {
  echo "ОШИБКА: признак стоп-релиза должен быть true или false" >&2
  exit 1
}

emit() {
  printf 'update=%s\nversion=%s\nxray=%s\nreason=%s\n' "$1" "$2" "$main_xray" "$3"
}

if [ "$stop" = true ]; then
  emit false "" "на открытом issue висит метка «стоп-релиз»"
elif [ -z "$last_tag" ]; then
  emit false "" "stable-выпусков ещё нет"
elif [ "$stable_xray" = "$main_xray" ]; then
  emit false "" "xray в последней stable уже $main_xray"
else
  [[ $last_tag =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+)(-dev\.[0-9]+)?$ ]] || {
    echo "ОШИБКА: прошлый тег «$last_tag» не вида vX.Y.Z" >&2
    exit 1
  }
  next="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.$((BASH_REMATCH[3] + 1))"
  [[ $stable_xray =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "ОШИБКА: версия xray прошлой stable не вида X.Y.Z: $stable_xray" >&2
    exit 1
  }
  newest=$(printf '%s\n' "$main_xray" "$stable_xray" | sort -V | tail -n 1)
  if [ "$newest" = "$main_xray" ]; then
    emit true "$next" "xray $main_xray новее xray $stable_xray в $last_tag"
  else
    emit false "" "xray $stable_xray в $last_tag новее versions.toml ($main_xray)"
  fi
fi
