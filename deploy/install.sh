#!/bin/sh
# Установщик raycat на сервер: бинарники, служба systemd и пример настроек.
#
#   curl -fsSL https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/install.sh | sudo sh
#   sh install.sh --help
#
# Скрипт ставит только файлы raycat и службу. Сетевые настройки хоста (sysctl,
# маршруты, правила) он не трогает: режим шлюза настраивает сам демон по файлу
# настроек.
#
# Переменные для тестов (в обычной работе не нужны):
#   RAYCAT_INSTALL_FROM=каталог        SHA256SUMS и архивы берутся из каталога, а не с GitHub
#   RAYCAT_INSTALL_CPUINFO=файл        вместо /proc/cpuinfo
#   RAYCAT_INSTALL_UNAME_M=значение    вместо вывода uname -m
set -eu

REPO=raycat-app/raycat
SITE=https://github.com/$REPO
API=https://api.github.com/repos/$REPO

BIN=/usr/local/bin/raycat
XRAY=/usr/libexec/raycat/xray
CONFIG_DIR=/etc/raycat
CONFIG=$CONFIG_DIR/config.toml
STATE_DIR=/var/lib/raycat
UNIT=/etc/systemd/system/raycat.service
COMP_BASH=/usr/local/share/bash-completion/completions/raycat
COMP_ZSH=/usr/local/share/zsh/site-functions/_raycat
COMP_FISH=/usr/local/share/fish/vendor_completions.d/raycat.fish
MAN_PAGE=/usr/local/share/man/man1/raycat.1
LICENSE_FILE=/usr/local/share/doc/raycat/LICENSE

channel=stable
version=
noaes=auto
start=1
attest=1
uninstall=
purge=
dry=
tmp=
from=
base=
dev_tag=
target=
suffix=
asset_version=
archive=
pkg_dir=
put_new=0
bins_changed=0

say() { printf '%s\n' "$*"; }
warn() { printf 'предупреждение: %s\n' "$*" >&2; }
die() {
  printf 'ошибка: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Установщик raycat.

Использование: install.sh [параметры]

Без параметров ставит последний stable-выпуск, включает службу systemd и, если
настройки уже есть, запускает её. Повторный запуск обновляет raycat; служба
перезапускается, только если файлы изменились.

Параметры:
  --channel stable|dev   канал выпусков (по умолчанию stable)
  --version X.Y.Z        конкретная версия (X.Y.Z или X.Y.Z-dev.N); сильнее --channel
  --noaes, --no-noaes    принудительно выбрать или не выбирать сборку xray для
                         процессоров без AES (aarch64 и armv7); по умолчанию
                         определяется по /proc/cpuinfo
  --no-start             не запускать и не перезапускать службу
  --no-attestation       не проверять происхождение архива через gh
  --dry-run              показать, что будет сделано, ничего не меняя
  --uninstall            удалить raycat; настройки и состояние сохраняются
  --purge                вместе с --uninstall удалить и настройки с состоянием
  -h, --help             эта справка

Нужны права root, curl или wget, tar и sha256sum (или shasum).

Раскладка:
  /usr/local/bin/raycat                       программа
  /usr/libexec/raycat/xray                    ядро xray (путь по умолчанию xray.path)
  /etc/raycat/config.toml                     настройки (0600, не перезаписываются)
  /var/lib/raycat                             состояние
  /etc/systemd/system/raycat.service          служба
  /usr/local/share/{bash-completion,zsh,fish,man,doc}/…   автодополнение, man, лицензия
EOF
}

cleanup() {
  if [ -n "$tmp" ]; then
    rm -rf "$tmp"
  fi
}

parse_args() {
  while [ $# -gt 0 ]; do
    case $1 in
      --channel | --version)
        [ $# -ge 2 ] || die "у параметра $1 нет значения (справка: --help)"
        set_option "$1" "$2"
        shift
        ;;
      --channel=* | --version=*) set_option "${1%%=*}" "${1#*=}" ;;
      --noaes) noaes=yes ;;
      --no-noaes) noaes=no ;;
      --no-start) start= ;;
      --no-attestation) attest= ;;
      --dry-run) dry=1 ;;
      --uninstall) uninstall=1 ;;
      --purge) purge=1 ;;
      -h | --help)
        usage
        exit 0
        ;;
      *) die "неизвестный параметр: $1 (справка: --help)" ;;
    esac
    shift
  done
  if [ -n "$purge" ] && [ -z "$uninstall" ]; then
    die "--purge работает только вместе с --uninstall"
  fi
}

set_option() {
  case $1 in
    --channel)
      case $2 in
        stable | dev) channel=$2 ;;
        *) die "неизвестный канал «$2»: допустимо stable или dev" ;;
      esac
      ;;
    --version)
      version=${2#v}
      if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-dev\.[0-9]+)?$'; then
        die "версия «$2» не похожа на X.Y.Z или X.Y.Z-dev.N"
      fi
      ;;
  esac
}

need_root() {
  if [ -n "$dry" ]; then
    return 0
  fi
  if [ "$(id -u)" != 0 ]; then
    die "нужны права root: запустите через sudo (например, curl -fsSL … | sudo sh)"
  fi
}

have() { command -v "$1" >/dev/null 2>&1; }

have_systemd() {
  [ -d /run/systemd/system ] && have systemctl
}

download() {
  case $1 in
    https://*) ;;
    *) die "внутренняя ошибка: ссылка не https: $1" ;;
  esac
  if have curl; then
    curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 \
      --connect-timeout 20 --max-filesize "$3" -o "$2" "$1"
  elif have wget; then
    if wget --help 2>&1 | grep -q -e --https-only; then
      wget -q --https-only -O "$2" "$1"
    else
      wget -q -O "$2" "$1"
    fi
  else
    die "нужен curl или wget, ни того ни другого нет"
  fi
}

# shellcheck disable=SC2016
sha256_of() {
  if have sha256sum; then
    sha256sum "$1" | awk '{ print $1 }'
  elif have shasum; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    die "нужен sha256sum (coreutils) или shasum"
  fi
}

fetch_asset() {
  if [ -n "$from" ]; then
    [ -f "$from/$1" ] || die "в каталоге $from нет файла $1"
    cp "$from/$1" "$2"
  else
    if ! download "$base/$1" "$2" "$3"; then
      case $base in
        */latest/download) die "не удалось скачать $base/$1: нет сети или stable-выпусков ещё нет (попробуйте --channel dev)" ;;
        *) die "не удалось скачать $base/$1: нет сети или такого выпуска" ;;
      esac
    fi
  fi
}

find_dev_tag() {
  download "$API/releases?per_page=30" "$tmp/releases.json" 1048576 ||
    die "не удалось получить список выпусков GitHub (возможно, лимит запросов): укажите версию через --version"
  dev_tag=$(tr ',' '\n' <"$tmp/releases.json" |
    sed -n 's/.*"tag_name": *"\(v[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*-dev\.[0-9][0-9]*\)".*/\1/p' |
    head -n 1)
  if [ -z "$dev_tag" ]; then
    die "dev-выпусков на GitHub не нашлось"
  fi
}

detect_target() {
  if [ "$(uname -s)" != Linux ]; then
    die "поддерживается только Linux"
  fi
  machine=${RAYCAT_INSTALL_UNAME_M:-$(uname -m)}
  arm=
  case $machine in
    x86_64 | amd64) target=x86_64-linux-musl ;;
    aarch64 | arm64)
      target=aarch64-linux-musl
      arm=1
      ;;
    armv7* | armv8l)
      target=armv7-linux-musleabihf
      arm=1
      ;;
    *) die "архитектура $machine не поддерживается (нужны x86_64, aarch64 или armv7)" ;;
  esac
  suffix=
  case $noaes in
    yes)
      [ -n "$arm" ] || die "сборка -noaes есть только для aarch64 и armv7"
      suffix=-noaes
      ;;
    no) ;;
    auto)
      if [ -n "$arm" ]; then
        cpuinfo=${RAYCAT_INSTALL_CPUINFO:-/proc/cpuinfo}
        if [ -r "$cpuinfo" ] && grep -Eqi '^Features[[:space:]]*:' "$cpuinfo" &&
          ! grep -Eqi '^Features[[:space:]]*:.*[[:space:]]aes([[:space:]]|$)' "$cpuinfo"; then
          suffix=-noaes
          say "В процессоре нет аппаратного AES: выбрана сборка xray -noaes (отменить: --no-noaes)."
        fi
      fi
      ;;
  esac
}

# shellcheck disable=SC2016
resolve_release() {
  from=${RAYCAT_INSTALL_FROM:-}
  if [ -n "$from" ]; then
    base=
    warn "режим испытаний: файлы берутся из каталога $from, а не с GitHub"
  elif [ -n "$version" ]; then
    base=$SITE/releases/download/v$version
  elif [ "$channel" = dev ]; then
    find_dev_tag
    base=$SITE/releases/download/$dev_tag
  else
    base=$SITE/releases/latest/download
  fi
  fetch_asset SHA256SUMS "$tmp/SHA256SUMS" 1048576
  versions=$(awk '{
    f = $2
    sub(/^\*/, "", f)
    if (match(f, /^raycat-[0-9]+\.[0-9]+\.[0-9]+(-dev\.[0-9]+)?-/)) {
      print substr(f, 8, RLENGTH - 8)
    }
  }' "$tmp/SHA256SUMS" | sort -u)
  case $versions in
    '') die "в SHA256SUMS нет архивов raycat" ;;
    *'
'*) die "в SHA256SUMS архивы разных версий: $(printf '%s' "$versions" | tr '\n' ' ')" ;;
  esac
  asset_version=$versions
  if [ -n "$from" ] && [ -n "$version" ] && [ "$version" != "$asset_version" ]; then
    die "в каталоге $from лежит версия $asset_version, а запрошена $version"
  fi
  archive=raycat-$asset_version-$target$suffix.tar.gz
}

# shellcheck disable=SC2016
verify_checksum() {
  want=$(awk -v n="$archive" '{
    f = $2
    sub(/^\*/, "", f)
    if (f == n) print $1
  }' "$tmp/SHA256SUMS")
  case $want in
    '' | *[!0-9a-f]*) die "в SHA256SUMS нет корректной контрольной суммы для $archive" ;;
  esac
  [ "${#want}" -eq 64 ] || die "в SHA256SUMS нет корректной контрольной суммы для $archive"
  have_sum=$(sha256_of "$tmp/$archive")
  if [ "$have_sum" != "$want" ]; then
    die "контрольная сумма $archive не совпала с SHA256SUMS: файл повреждён или подменён. Система не изменена."
  fi
  say "Контрольная сумма совпала."
}

verify_attestation() {
  if [ -z "$attest" ]; then
    say "Проверка происхождения отключена (--no-attestation)."
    return 0
  fi
  if [ -n "$from" ]; then
    return 0
  fi
  if ! have gh; then
    say "gh не найден: проверка происхождения архива пропущена (с gh: gh attestation verify)."
    return 0
  fi
  if ! gh auth status >/dev/null 2>&1; then
    say "gh не авторизован для этого пользователя: проверка происхождения пропущена."
    return 0
  fi
  if ! gh attestation verify "$tmp/$archive" --repo "$REPO" >/dev/null 2>&1; then
    die "gh не подтвердил происхождение $archive из репозитория $REPO. Система не изменена (пропустить проверку: --no-attestation)."
  fi
  say "Происхождение архива подтверждено (gh attestation verify)."
}

# shellcheck disable=SC2016
unpack() {
  dir=raycat-$asset_version-$target$suffix
  tar -tzf "$tmp/$archive" >"$tmp/names" 2>/dev/null || die "архив $archive не читается"
  tar -tvzf "$tmp/$archive" >"$tmp/types" 2>/dev/null || die "архив $archive не читается"
  if ! awk -v d="$dir" '{
    if ($0 != d && substr($0, 1, length(d) + 1) != d "/") bad = 1
    if ($0 ~ /(^|\/)\.\.(\/|$)/) bad = 1
  } END { exit bad }' "$tmp/names"; then
    die "в архиве пути вне каталога $dir: он не принят"
  fi
  if ! awk '{ t = substr($0, 1, 1); if (t != "-" && t != "d") bad = 1 } END { exit bad }' "$tmp/types"; then
    die "в архиве есть ссылки или специальные файлы: он не принят"
  fi
  mkdir "$tmp/pkg"
  tar -xzf "$tmp/$archive" -C "$tmp/pkg" || die "не удалось распаковать $archive"
  pkg_dir=$tmp/pkg/$dir
  for need in raycat xray systemd/raycat.service; do
    if [ ! -f "$pkg_dir/$need" ] || [ -L "$pkg_dir/$need" ]; then
      die "в архиве нет файла $need"
    fi
  done
}

put() {
  put_new=0
  if [ -f "$3" ] && [ ! -L "$3" ] && [ "$(sha256_of "$1")" = "$(sha256_of "$3")" ]; then
    chmod "$2" "$3"
    return 0
  fi
  put_new=1
  mkdir -p "$(dirname "$3")"
  install -m "$2" "$1" "$3.new.$$"
  mv -f "$3.new.$$" "$3"
}

put_program() {
  put "$@"
  if [ "$put_new" = 1 ]; then
    bins_changed=1
  fi
}

put_optional() {
  if [ -f "$1" ] && [ ! -L "$1" ]; then
    put "$1" 0644 "$2"
  fi
}

write_example_config() {
  cat <<'EOF'
#:schema https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/config.schema.json
# Настройки raycat: /etc/raycat/config.toml (права 0600, в файле ссылка подписки).
# После правки: sudo systemctl restart raycat
# Проверка файла и подписок: sudo raycat check
# Описание всех ключей: https://github.com/raycat-app/raycat

[[subscription]]
name = "основная"
# Ссылка подписки работает как пароль: никому её не показывайте.
url = "https://example.com/sub/REPLACE-WITH-YOUR-LINK"
# Какое приложение провайдер считает клиентом: happ (windows или android)
# либо incy (только android). Выберите то, которое он принимает.
app = "happ"
platform = "windows"

# Ещё подписки: добавьте такие же блоки [[subscription]]. Первая основная,
# остальные резервные.

# По умолчанию работает прокси для программ на этом сервере: HTTP и SOCKS5 на
# 127.0.0.1:7890. Чтобы весь исходящий трафик сервера и его контейнеров шёл через
# VPN, включите шлюз (нужны nftables и iproute2):
#
# [mode]
# type = "gateway"
# kill_switch = true

# Одно и то же слово даёт одно и то же устройство у провайдера на любом сервере.
# Без него устройство создаётся при первом запуске и хранится в /var/lib/raycat.
#
# [device]
# seed = "любое слово или фраза"
EOF
}

print_plan() {
  say "План (--dry-run, система не изменена):"
  say "  версия:       $asset_version, сборка $target$suffix"
  if [ -n "$from" ]; then
    say "  источник:     каталог $from"
  else
    say "  источник:     $base/$archive"
  fi
  say "  проверка:     контрольная сумма по SHA256SUMS$(plan_attestation)"
  say "  установить:   $BIN"
  say "                $XRAY"
  say "                $COMP_BASH, $COMP_ZSH, $COMP_FISH (если есть в архиве)"
  say "                $MAN_PAGE, $LICENSE_FILE (если есть в архиве)"
  if [ -e "$CONFIG" ]; then
    say "  настройки:    $CONFIG уже есть, не меняется"
  else
    say "  настройки:    создать $CONFIG (0600) из примера"
  fi
  if have_systemd; then
    say "  служба:       $UNIT, systemctl daemon-reload, enable"
    if [ -z "$start" ]; then
      say "                запуск не выполняется (--no-start)"
    elif [ ! -e "$CONFIG" ]; then
      say "                без запуска: сначала укажите ссылку подписки: sudo raycat init --force"
    elif [ ! -e "$UNIT" ]; then
      say "                запустить (systemctl start raycat)"
    else
      say "                перезапустить, только если файлы изменятся"
    fi
  else
    say "  служба:       systemd не найден, служба не настраивается"
  fi
}

plan_attestation() {
  if [ -z "$attest" ] || [ -n "$from" ]; then
    return 0
  fi
  if have gh && gh auth status >/dev/null 2>&1; then
    printf ' и gh attestation verify'
  fi
}

service_is_active() {
  systemctl is-active --quiet raycat
}

wait_active() {
  tries=0
  while [ "$tries" -lt 5 ]; do
    if service_is_active; then
      return 0
    fi
    tries=$((tries + 1))
    sleep 1
  done
  return 1
}

setup_service() {
  was_installed=1
  if [ ! -e "$UNIT" ]; then
    was_installed=0
  fi
  put "$pkg_dir/systemd/raycat.service" 0644 "$UNIT"
  unit_changed=$put_new
  if [ "$unit_changed" = 1 ]; then
    systemctl daemon-reload
  fi
  systemctl enable raycat >/dev/null 2>&1 || die "не удалось включить службу: systemctl enable raycat"
  if [ -z "$start" ]; then
    say "Служба включена, но не запущена и не перезапущена (--no-start)."
  elif [ -n "$fresh_config" ]; then
    say "Служба включена, но пока не запущена. После указания ссылки подписки выполните:"
    say "  sudo systemctl start raycat"
  elif [ "$was_installed" = 0 ]; then
    systemctl start raycat || true
    report_start
  elif [ "$bins_changed" = 1 ] || [ "$unit_changed" = 1 ]; then
    if service_is_active; then
      systemctl restart raycat || true
      report_start
    else
      say "Служба остановлена и остаётся остановленной: sudo systemctl start raycat"
    fi
  elif service_is_active; then
    say "Файлы не изменились, службу перезапускать не нужно."
  else
    say "Служба остановлена: sudo systemctl start raycat"
  fi
}

report_start() {
  if wait_active; then
    say "Служба raycat запущена."
  else
    warn "служба не поднялась: смотрите journalctl -u raycat -n 50 --no-pager"
  fi
}

check_gateway_tools() {
  missing=
  for tool in nft ip; do
    if ! have "$tool"; then
      missing="$missing $tool"
    fi
  done
  if [ -n "$missing" ]; then
    say "Для режима шлюза нужны nftables и iproute2 (не найдены:$missing); режиму прокси они не нужны."
  fi
}

do_install() {
  need_root
  for tool in tar awk sed grep tr sort head install mktemp; do
    have "$tool" || die "нужна команда $tool, её нет в системе"
  done
  detect_target
  tmp=$(mktemp -d) || die "не удалось создать временный каталог"
  resolve_release
  if [ -n "$dry" ]; then
    print_plan
    return 0
  fi

  say "Устанавливаю raycat $asset_version ($target$suffix)."
  fetch_asset "$archive" "$tmp/$archive" 209715200
  verify_checksum
  verify_attestation
  unpack

  put_program "$pkg_dir/xray" 0755 "$XRAY"
  put_program "$pkg_dir/raycat" 0755 "$BIN"
  put_optional "$pkg_dir/completions/raycat.bash" "$COMP_BASH"
  put_optional "$pkg_dir/completions/_raycat" "$COMP_ZSH"
  put_optional "$pkg_dir/completions/raycat.fish" "$COMP_FISH"
  put_optional "$pkg_dir/man/raycat.1" "$MAN_PAGE"
  put_optional "$pkg_dir/LICENSE" "$LICENSE_FILE"

  fresh_config=
  if [ -e "$CONFIG" ]; then
    say "Настройки $CONFIG сохранены без изменений."
  else
    install -d -m 0700 "$CONFIG_DIR"
    write_example_config >"$tmp/config.toml"
    install -m 0600 "$tmp/config.toml" "$CONFIG"
    fresh_config=1
    say "Создан пример настроек $CONFIG. Укажите ссылку подписки: sudo raycat init --force"
  fi

  if have_systemd; then
    setup_service
  else
    say "systemd не найден: служба не настроена. Запуск вручную от root:"
    say "  $BIN daemon"
    say "Автозапуск настройте средствами вашей init-системы. Если демон уже запущен"
    say "вручную, перезапустите его, чтобы он начал использовать новые файлы."
  fi
  check_gateway_tools

  if running=$("$BIN" --version 2>&1); then
    label=$running
  else
    warn "$BIN не запускается ($running): подходит ли сборка этому процессору и ядру?"
    label=raycat
  fi
  say ""
  say "Готово: $label установлен."
  say "  настройки:  $CONFIG"
  say "  состояние:  $STATE_DIR"
  if have_systemd; then
    say "  журнал:     journalctl -u raycat -f"
  fi
  say "  команды:    sudo raycat status | nodes | tui (справка: raycat --help)"
  say "  обновление: повторите установку; удаление: install.sh --uninstall"
}

act() {
  if [ -n "$dry" ]; then
    say "  выполнить: $*"
  else
    "$@"
  fi
}

do_uninstall() {
  need_root
  if [ -n "$dry" ]; then
    say "План (--dry-run, система не изменена):"
  fi
  if have_systemd; then
    if [ -e "$UNIT" ] || systemctl cat raycat >/dev/null 2>&1; then
      act systemctl disable --now raycat || true
    fi
    act rm -f "$UNIT"
    act systemctl daemon-reload
  else
    say "systemd не найден: если демон запущен вручную, остановите его сами."
  fi
  act rm -f "$BIN" "$XRAY" "$COMP_BASH" "$COMP_ZSH" "$COMP_FISH" "$MAN_PAGE" "$LICENSE_FILE"
  act rmdir /usr/libexec/raycat /usr/local/share/doc/raycat 2>/dev/null || true
  if [ -n "$purge" ]; then
    act rm -rf "$CONFIG_DIR" "$STATE_DIR"
    say "Настройки и состояние удалены; устройство для провайдера при новой установке создастся заново."
  else
    say "Настройки ($CONFIG_DIR) и состояние ($STATE_DIR) сохранены; удалить их тоже: --uninstall --purge."
  fi
  if [ -z "$dry" ]; then
    say "raycat удалён."
  fi
}

main() {
  exec </dev/null
  umask 022
  trap cleanup EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  parse_args "$@"
  if [ -n "$uninstall" ]; then
    do_uninstall
  else
    do_install
  fi
}

main "$@"
