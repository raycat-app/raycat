# Выбор релиза для захвата (подключается из watch.sh и capture.yml). Версия — номер в теге
# (desktop-v3.8.8, android-v3.7.9, 4.5.4). Тег без номера (плавающий «latest» у INCY) версией
# не считается. Сравниваются номера, а не строки тегов: у одного приложения разные префиксы.

version_of() {
  local tag=$1
  printf '%s' "${tag#"${tag%%[0-9]*}"}"
}

# $1 — репозиторий, $2 — файл релиза, по которому релиз узнаётся как релиз приложения.
# Черновики и предрелизы (в Happ это бета-сборки) профилем не становятся.
latest_stable() {
  gh api "repos/$1/releases?per_page=100" |
    jq -r --arg asset "$2" '.[] | select(.draft == false and .prerelease == false) | select(any(.assets[]; .name == $asset)) | .tag_name | select(test("[0-9]"))' |
    while read -r tag; do printf '%s\t%s\n' "$(version_of "$tag")" "$tag"; done |
    sort -V | tail -n 1 | cut -f2
}
