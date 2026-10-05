# shellcheck shell=bash
# Общие функции скриптов выпуска; подключается через source.

# shellcheck disable=SC2034
ARCHIVE_SUFFIXES=(
  x86_64-linux-musl
  aarch64-linux-musl
  aarch64-linux-musl-noaes
  armv7-linux-musleabihf
  armv7-linux-musleabihf-noaes
)

fail() {
  echo "ОШИБКА: $*" >&2
  exit 1
}

# pack <каталог> <имя> <файл .tar.gz>: воспроизводимый архив каталога <имя> из <каталога>.
# Порядок, владелец и время заданы, поэтому один набор файлов даёт одни и те же байты.
pack() {
  [ -n "${SOURCE_DATE_EPOCH:-}" ] || fail "SOURCE_DATE_EPOCH не задан"
  LC_ALL=C tar --sort=name --owner=0 --group=0 --numeric-owner --format=gnu \
    --mtime="@${SOURCE_DATE_EPOCH}" -C "$1" -cf - "$2" | gzip -n -9 >"$3"
}

# fingerprint <каталог>: хэши и права всех файлов каталога для сравнения содержимого.
fingerprint() {
  (
    cd "$1" || exit 1
    find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum
    find . -printf '%m %y %p\n' | LC_ALL=C sort
  )
}

# digest_of <образ>: digest манифеста по тегу или ссылке.
digest_of() {
  echo "sha256:$(docker buildx imagetools inspect --raw "$1" | sha256sum | cut -d' ' -f1)"
}

# retag <репозиторий> <digest> <тег>...: новые теги на тот же манифест без изменения digest.
# imagetools create заворачивает одиночный образ в новый список, поэтому копирует skopeo.
retag() {
  local repo=$1 digest=$2 tag
  shift 2
  local args=(--all --preserve-digests)
  if [ "${INSECURE_REGISTRY:-}" = true ]; then
    args+=(--src-tls-verify=false --dest-tls-verify=false)
  fi
  for tag in "$@"; do
    skopeo copy "${args[@]}" "docker://$repo@$digest" "docker://$repo:$tag"
  done
}
