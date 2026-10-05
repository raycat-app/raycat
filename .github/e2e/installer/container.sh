#!/bin/sh
# e2e установщика в контейнере без systemd (debian, alpine): раскладка, идемпотентность,
# обновление, удаление, отказ при испорченных архивах, выбор сборки noaes.
#
# Запускается внутри контейнера от root. Нужны каталоги:
#   /repo/deploy   deploy/ из репозитория
#   /release       ресурсы выпуска 0.9.0 (.github/e2e/installer/release.sh)
#   /release2      ресурсы выпуска 0.9.1, xray на байт длиннее
set -eu

installer=/repo/deploy/install.sh

fail() {
  printf 'ПРОВАЛ: %s\n' "$*" >&2
  exit 1
}

inst() { env RAYCAT_INSTALL_FROM=/release sh "$installer" "$@"; }
inst_from() {
  from=$1
  shift
  env "RAYCAT_INSTALL_FROM=$from" sh "$installer" "$@"
}

installed() {
  [ -e /usr/local/bin/raycat ] || [ -e /usr/libexec/raycat/xray ] || [ -e /usr/local/share/man/man1/raycat.1 ]
}

# refuses «текст» команда…: команда обязана упасть, а сообщение содержать текст.
refuses() {
  text=$1
  shift
  if out=$("$@" 2>&1); then
    fail "должно завершиться ошибкой: $*"
  fi
  printf '%s\n' "$out" | grep -q -- "$text" || fail "в сообщении «$*» нет «$text»: $out"
}

# expect «текст» вывод: вывод обязан содержать текст.
expect() {
  printf '%s\n' "$2" | grep -q -- "$1" || fail "в выводе нет «$1»: $2"
}

inode() { stat -c %i "$1"; }

reset() { inst --uninstall --purge >/dev/null 2>&1 || fail "не удалось сбросить состояние"; }

echo "== параметры"
out=$(inst --help) || fail "--help завершилась ошибкой"
expect '--uninstall' "$out"
refuses 'неизвестный параметр' inst --nope
refuses 'только вместе с --uninstall' inst --purge
refuses 'неизвестный канал' inst --channel beta
refuses 'не похожа' inst --version 1.2
refuses 'не похожа' inst --version '1.2.3;id'
refuses 'нет значения' inst --version
refuses 'не поддерживается' env RAYCAT_INSTALL_UNAME_M=mips64 RAYCAT_INSTALL_FROM=/release sh "$installer"
refuses 'только для aarch64 и armv7' inst --noaes

echo "== без root"
if command -v su >/dev/null 2>&1; then
  out=$(su -s /bin/sh nobody -c "sh $installer" 2>&1) && fail "установка без root должна упасть"
  expect 'нужны права root' "$out"
fi
installed && fail "после отказа без root что-то установилось"

echo "== --dry-run ничего не меняет"
out=$(inst --dry-run 2>&1) || fail "--dry-run завершилась ошибкой: $out"
expect 'План' "$out"
expect '0.9.0' "$out"
expect 'x86_64-linux-musl' "$out"
installed && fail "--dry-run установил файлы"
[ ! -e /etc/raycat ] || fail "--dry-run создал /etc/raycat"
out=$(inst --uninstall --dry-run 2>&1) || fail "--uninstall --dry-run завершилась ошибкой: $out"
expect 'План' "$out"

echo "== установка"
out=$(inst 2>&1) || fail "установка не удалась: $out"
expect 'Контрольная сумма совпала' "$out"
expect 'systemd не найден' "$out"
for file in /usr/local/bin/raycat /usr/libexec/raycat/xray; do
  [ -x "$file" ] || fail "нет исполняемого файла $file"
done
for file in /usr/local/share/bash-completion/completions/raycat /usr/local/share/zsh/site-functions/_raycat \
  /usr/local/share/fish/vendor_completions.d/raycat.fish /usr/local/share/man/man1/raycat.1 \
  /usr/local/share/doc/raycat/LICENSE; do
  [ -s "$file" ] || fail "нет файла $file"
done
[ ! -e /etc/systemd/system/raycat.service ] || fail "юнит поставлен без systemd"
[ "$(stat -c %a /etc/raycat/config.toml)" = 600 ] || fail "права настроек не 0600"
[ "$(stat -c %a /etc/raycat)" = 700 ] || fail "права /etc/raycat не 0700"
cmp /etc/raycat/config.toml /repo/deploy/config.example.toml ||
  fail "пример настроек в install.sh разошёлся с deploy/config.example.toml"
raycat --version | grep -q '^raycat ' || fail "raycat --version: $(raycat --version)"
/usr/libexec/raycat/xray version | grep -q '^Xray ' || fail "xray version не сработал"
state=$(mktemp -d)
env "RAYCAT_STATE_DIR=$state" raycat identity >/dev/null || fail "raycat не принял пример настроек"

echo "== повторный запуск: ничего не меняется"
echo '# своя строка' >>/etc/raycat/config.toml
raycat_inode=$(inode /usr/local/bin/raycat)
xray_inode=$(inode /usr/libexec/raycat/xray)
out=$(inst 2>&1) || fail "повторный запуск не удался: $out"
expect 'сохранены без изменений' "$out"
grep -q '^# своя строка$' /etc/raycat/config.toml || fail "настройки перезаписаны"
[ "$raycat_inode" = "$(inode /usr/local/bin/raycat)" ] || fail "raycat заменён без изменений"
[ "$xray_inode" = "$(inode /usr/libexec/raycat/xray)" ] || fail "xray заменён без изменений"
for leftover in /usr/local/bin /usr/libexec/raycat /usr/local/share/man/man1 /usr/local/share/doc/raycat; do
  [ -z "$(find "$leftover" -name '*.new.*')" ] || fail "в $leftover остались временные файлы"
done

echo "== обновление"
old_size=$(wc -c </usr/libexec/raycat/xray)
out=$(inst_from /release2 2>&1) || fail "обновление не удалось: $out"
expect '0.9.1' "$out"
[ "$(wc -c </usr/libexec/raycat/xray)" -eq $((old_size + 1)) ] || fail "xray не обновился"
[ "$raycat_inode" = "$(inode /usr/local/bin/raycat)" ] || fail "raycat заменён, хотя не менялся"
grep -q '^# своя строка$' /etc/raycat/config.toml || fail "настройки потеряны при обновлении"
refuses 'лежит версия 0.9.1' inst_from /release2 --version 0.9.0
inst_from /release2 --version 0.9.1 >/dev/null 2>&1 || fail "--version 0.9.1 должна подойти"

echo "== удаление сохраняет настройки и состояние"
mkdir -p /var/lib/raycat
echo state >/var/lib/raycat/machine-id
out=$(inst --uninstall 2>&1) || fail "удаление не удалось: $out"
installed && fail "после --uninstall остались файлы"
[ ! -e /usr/libexec/raycat ] || fail "остался каталог /usr/libexec/raycat"
[ ! -e /usr/local/share/doc/raycat ] || fail "остался каталог документации"
[ -f /etc/raycat/config.toml ] || fail "--uninstall удалил настройки"
[ -f /var/lib/raycat/machine-id ] || fail "--uninstall удалил состояние"
inst --uninstall >/dev/null 2>&1 || fail "повторное удаление должно проходить"

echo "== --purge удаляет всё"
inst >/dev/null 2>&1 || fail "переустановка не удалась"
inst --uninstall --purge >/dev/null 2>&1 || fail "--uninstall --purge не удалась"
[ ! -e /etc/raycat ] || fail "--purge оставил /etc/raycat"
[ ! -e /var/lib/raycat ] || fail "--purge оставил /var/lib/raycat"
installed && fail "--purge оставил файлы"

echo "== испорченные ресурсы: отказ без изменений"
bad=$(mktemp -d)
cp /release/* "$bad/"
zeros=$(printf '%064d' 0)
sed "s/^[0-9a-f]\{64\}\( .*-x86_64-linux-musl\.tar\.gz\)\$/$zeros\1/" /release/SHA256SUMS >"$bad/SHA256SUMS"
refuses 'контрольная сумма .* не совпала' inst_from "$bad"
installed && fail "при неверной SHA256SUMS что-то установилось"
[ ! -e /etc/raycat ] || fail "при неверной SHA256SUMS создан /etc/raycat"
cp /release/SHA256SUMS "$bad/SHA256SUMS"
printf 'x' >>"$bad/raycat-0.9.0-x86_64-linux-musl.tar.gz"
refuses 'контрольная сумма .* не совпала' inst_from "$bad"
installed && fail "при испорченном архиве что-то установилось"
grep -v 'x86_64' /release/SHA256SUMS >"$bad/SHA256SUMS"
refuses 'нет корректной контрольной суммы' inst_from "$bad"
cat /release/SHA256SUMS /release2/SHA256SUMS >"$bad/SHA256SUMS"
refuses 'разных версий' inst_from "$bad"
: >"$bad/SHA256SUMS"
refuses 'нет архивов raycat' inst_from "$bad"
installed && fail "после отказов что-то установилось"

echo "== опасные архивы"
evil=$(mktemp -d)
name=raycat-0.9.9-x86_64-linux-musl
(
  cd "$evil"
  mkdir -p "$name/systemd"
  ln -s /etc/passwd "$name/raycat"
  echo x >"$name/xray"
  echo x >"$name/systemd/raycat.service"
  tar -czf "$name.tar.gz" "$name"
  sha256sum "$name.tar.gz" >SHA256SUMS
)
refuses 'ссылки' inst_from "$evil"
(
  cd "$evil"
  rm -rf "$name"
  mkdir -p "$name/systemd"
  echo x >"$name/raycat"
  echo x >"$name/xray"
  echo x >"$name/systemd/raycat.service"
  echo stray >stray
  tar -czf "$name.tar.gz" "$name" stray
  sha256sum "$name.tar.gz" >SHA256SUMS
)
refuses 'вне каталога' inst_from "$evil"
installed && fail "опасный архив что-то установил"

echo "== выбор сборки по процессору"
printf 'processor\t: 0\nFeatures\t: fp asimd evtstrm aes pmull sha1 sha2 crc32 cpuid\n' >/tmp/cpu-aes
printf 'processor\t: 0\nFeatures\t: fp asimd evtstrm crc32 cpuid\n' >/tmp/cpu-noaes
printf 'processor\t: 0\nflags\t\t: fpu vme sse sse2\n' >/tmp/cpu-x86

# picks «uname -m» файл_cpuinfo ожидаемая_цель [параметры…]
picks() {
  machine=$1
  cpuinfo=$2
  want=$3
  shift 3
  out=$(env RAYCAT_INSTALL_FROM=/release "RAYCAT_INSTALL_UNAME_M=$machine" "RAYCAT_INSTALL_CPUINFO=$cpuinfo" \
    sh "$installer" "$@" 2>&1) || fail "установка ($machine, $cpuinfo, $*) не удалась: $out"
  got=$(/usr/libexec/raycat/xray)
  [ "$got" = "xray $want" ] || fail "($machine, $cpuinfo, $*): поставлена «$got», ожидалась «xray $want»"
  reset
}

picks aarch64 /tmp/cpu-aes aarch64-linux-musl
picks aarch64 /tmp/cpu-noaes aarch64-linux-musl-noaes
picks arm64 /tmp/cpu-noaes aarch64-linux-musl-noaes
picks armv7l /tmp/cpu-noaes armv7-linux-musleabihf-noaes
picks armv7l /tmp/cpu-aes armv7-linux-musleabihf
picks armv8l /tmp/cpu-aes armv7-linux-musleabihf
picks aarch64 /tmp/cpu-noaes aarch64-linux-musl --no-noaes
picks aarch64 /tmp/cpu-aes aarch64-linux-musl-noaes --noaes
picks aarch64 /tmp/cpu-x86 aarch64-linux-musl
picks aarch64 /tmp/нет-такого-файла aarch64-linux-musl
out=$(env RAYCAT_INSTALL_FROM=/release RAYCAT_INSTALL_UNAME_M=aarch64 RAYCAT_INSTALL_CPUINFO=/tmp/cpu-noaes \
  sh "$installer" --dry-run 2>&1) || fail "--dry-run для aarch64 не удался: $out"
expect 'aarch64-linux-musl-noaes' "$out"
installed && fail "--dry-run для aarch64 установил файлы"

echo "== поиск dev-выпуска в ответе GitHub API"
# shellcheck disable=SC2016
[ "$(tail -n 1 "$installer")" = 'main "$@"' ] || fail "последняя строка install.sh должна быть вызовом main"
sed '$d' "$installer" >/tmp/install-lib.sh
cat >/tmp/releases-pretty.json <<'EOF'
[
  {
    "url": "https://api.github.com/repos/raycat-app/raycat/releases/3",
    "tag_name": "v0.3.0-dev.58",
    "draft": false,
    "prerelease": true
  },
  {
    "tag_name": "v0.3.0-dev.57",
    "prerelease": true
  },
  {
    "tag_name": "v0.2.0",
    "prerelease": false
  }
]
EOF
printf '[{"tag_name":"v0.3.0-dev.9","draft":false},{"tag_name":"v0.2.0"}]' >/tmp/releases-compact.json
printf '[{"tag_name":"v0.2.0"},{"tag_name":"v0.1.0"}]' >/tmp/releases-stable.json
find_tag() (
  fixture=$1
  # shellcheck disable=SC1091
  . /tmp/install-lib.sh
  tmp=$(mktemp -d)
  download() { cp "$fixture" "$2"; }
  find_dev_tag
  printf '%s' "$dev_tag"
)
[ "$(find_tag /tmp/releases-pretty.json)" = v0.3.0-dev.58 ] || fail "dev-выпуск (форматированный JSON) найден неверно"
[ "$(find_tag /tmp/releases-compact.json)" = v0.3.0-dev.9 ] || fail "dev-выпуск (сжатый JSON) найден неверно"
if find_tag /tmp/releases-stable.json >/dev/null 2>&1; then
  fail "без dev-выпусков поиск должен упасть"
fi

echo "== загрузка с GitHub: понятная ошибка для несуществующего выпуска"
ID=unknown
# shellcheck disable=SC1091
. /etc/os-release
check_download() {
  refuses 'не удалось скачать' sh "$installer" --version 0.0.1 --dry-run
}
case $ID in
  debian)
    refuses 'нужен curl или wget' sh "$installer" --version 0.0.1 --dry-run
    apt-get update -qq >/dev/null
    apt-get install -y -qq curl ca-certificates >/dev/null
    check_download
    ;;
  alpine)
    check_download
    apk add --no-cache -q curl >/dev/null
    check_download
    ;;
esac

echo "Все проверки установщика пройдены"
