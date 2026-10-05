#!/usr/bin/env bash
# e2e службы systemd на хосте с systemd (нужен sudo, nftables и python3): установка
# установщиком, проверка юнита, песочница с привилегиями шлюза, настоящий xray под
# песочницей, повторный запуск без перезапуска, обновление, падение, удаление.
#
#   bash .github/e2e/installer/systemd.sh <выпуск 0.9.0> <выпуск 0.9.1> [порог systemd-analyze security]
#
# Выпуски собирает release.sh. Узел, сайт и панель те же, что в e2e прокси:
# адреса на lo нужны, потому что xray не ходит напрямую к зарезервированным диапазонам.
set -euo pipefail

root=$(cd "$(dirname "$0")/../../.." && pwd)
release1=${1:?не указан каталог выпуска 0.9.0}
release2=${2:?не указан каталог выпуска 0.9.1}
max_exposure=${3:-25}

node_ip=11.11.11.10
node_port=18388
site_ip=11.11.11.20
site_port=18080
panel_port=18090
proxy=127.0.0.1:7890
password=e2e-password
installer="$root/deploy/install.sh"
caps_net_admin=0000000000001000

work=$(mktemp -d)
pids=()

fail() {
  echo "ПРОВАЛ: $*" >&2
  exit 1
}

cleanup() {
  local status=$?
  set +e
  if [ "$status" -ne 0 ]; then
    echo "::group::journalctl -u raycat"
    sudo journalctl -u raycat --no-pager -n 100
    echo "::endgroup::"
    echo "::group::journalctl -u raycat-probe"
    sudo journalctl -u raycat-probe --no-pager -n 50
    echo "::endgroup::"
  fi
  sudo systemctl stop raycat raycat-probe 2>/dev/null
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null; done
  wait 2>/dev/null
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT

wait_for() {
  local what=$1
  shift
  local _
  for _ in $(seq 30); do
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  fail "не дождались: $what"
}

install_from() {
  local release=$1
  shift
  sudo env "RAYCAT_INSTALL_FROM=$release" sh "$installer" "$@" 2>&1
}

main_pid() { systemctl show raycat --property MainPID --value; }
via_http() { curl -fsS --max-time 5 -x "http://$proxy" "http://$site_ip:$site_port/index.html"; }
site_ok() { [ "$(via_http)" = raycat-e2e-ok ]; }
status_is() { sudo raycat status --json | jq -e "$1"; }
xray_pid() { pgrep -f 'xray run -c /var/lib/raycat/xray.json' | head -n 1; }
restarted() {
  local now
  now=$(main_pid)
  [ "$now" != "$1" ] && [ "$now" != 0 ]
}
caps_of() { sed -n 's/^CapEff:[[:space:]]*//p' "/proc/$1/status"; }

echo "== --dry-run на хосте ничего не меняет"
out=$(RAYCAT_INSTALL_FROM=$release1 sh "$installer" --dry-run 2>&1) || fail "--dry-run: $out"
grep -q 'raycat.service' <<<"$out" || fail "в плане нет юнита: $out"
[ ! -e /etc/systemd/system/raycat.service ] || fail "--dry-run поставил юнит"

echo "== установка: юнит на месте, служба включена, но не запущена"
out=$(install_from "$release1") || fail "установка не удалась: $out"
grep -q 'пока не запущена' <<<"$out" || fail "нет подсказки про запуск: $out"
cmp /etc/systemd/system/raycat.service "$root/deploy/raycat.service" || fail "юнит отличается от deploy/raycat.service"
systemctl is-enabled --quiet raycat || fail "служба не включена"
if systemctl is-active --quiet raycat; then fail "служба запущена с примером настроек"; fi

echo "== юнит: verify и security"
sudo systemd-analyze verify /etc/systemd/system/raycat.service || fail "systemd-analyze verify"
sudo systemd-analyze security raycat.service --no-pager --threshold="$max_exposure" ||
  fail "оценка песочницы хуже порога $max_exposure"
for property in MemoryDenyWriteExecute=yes NoNewPrivileges=yes ProtectSystem=strict; do
  [ "$(systemctl show raycat --property "${property%%=*}" --value)" = "${property#*=}" ] ||
    fail "свойство ${property%%=*} не ${property#*=}"
done

echo "== песочница: привилегии шлюза хватает, остального нет"
cat >"$work/probe.sh" <<'EOF'
#!/bin/sh
set -eu
caps=$(sed -n 's/^CapEff:[[:space:]]*//p' /proc/self/status)
bound=$(sed -n 's/^CapBnd:[[:space:]]*//p' /proc/self/status)
echo "CapEff=$caps CapBnd=$bound"
[ "$caps" = 0000000000001000 ] || { echo "ожидалась только CAP_NET_ADMIN"; exit 1; }
[ "$bound" = 0000000000001000 ] || { echo "ограничивающий набор шире CAP_NET_ADMIN"; exit 1; }
grep -q '^NoNewPrivs:[[:space:]]*1' /proc/self/status
nft add table inet raycatprobe
nft delete table inet raycatprobe
ip -4 rule add fwmark 0x7a7a lookup 7999 priority 7999
ip -4 rule del fwmark 0x7a7a lookup 7999 priority 7999
ip -4 route add local 0.0.0.0/0 dev lo table 7999
ip -4 route flush table 7999
python3 -c 'import socket; s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_MARK, 0x52430000); s.setsockopt(socket.SOL_IP, socket.IP_TRANSPARENT, 1)'
if touch /etc/raycat-probe 2>/dev/null; then echo "запись в /etc разрешена"; exit 1; fi
if [ -n "$(ls -A /home 2>/dev/null)" ]; then echo "/home виден"; exit 1; fi
if cat /proc/sys/net/ipv4/ip_forward 2>/dev/null >/proc/sys/net/ipv4/ip_forward; then echo "sysctl доступен для записи"; exit 1; fi
echo "песочница в порядке"
EOF
sudo install -m 755 "$work/probe.sh" /opt/raycat-probe.sh
# shellcheck disable=SC2016
sed -e 's|^ExecStart=.*|ExecStart=/bin/sh /opt/raycat-probe.sh|' \
  -e 's|^Type=simple|Type=oneshot|' \
  -e '/^Restart/d' -e '/^StartLimit/d' -e '/^Environment/d' \
  -e '/^StateDirectory/d' -e '/^RuntimeDirectory/d' -e '/^\[Install\]/,$d' \
  "$root/deploy/raycat.service" | sudo tee /etc/systemd/system/raycat-probe.service >/dev/null
sudo systemctl daemon-reload
if ! sudo systemctl start raycat-probe.service; then
  sudo journalctl -u raycat-probe --no-pager -n 50 -o cat
  fail "проба песочницы не прошла"
fi
sudo journalctl -u raycat-probe --no-pager -n 20 -o cat
sudo rm -f /etc/systemd/system/raycat-probe.service /opt/raycat-probe.sh
sudo systemctl daemon-reload

echo "== узел, сайт и панель"
"/usr/libexec/raycat/xray" version
sudo ip addr add "$node_ip/32" dev lo
sudo ip addr add "$site_ip/32" dev lo
cat >"$work/node.json" <<EOF
{
  "log": {"loglevel": "info"},
  "inbounds": [{
    "listen": "$node_ip",
    "port": $node_port,
    "protocol": "shadowsocks",
    "settings": {"method": "aes-128-gcm", "password": "$password", "network": "tcp,udp"}
  }],
  "outbounds": [{"protocol": "freedom"}]
}
EOF
/usr/libexec/raycat/xray run -c "$work/node.json" >"$work/node.log" 2>&1 &
pids+=($!)
mkdir "$work/site"
echo raycat-e2e-ok >"$work/site/index.html"
python3 -m http.server "$site_port" --bind "$site_ip" --directory "$work/site" >"$work/site.log" 2>&1 &
pids+=($!)
userinfo=$(printf 'aes-128-gcm:%s' "$password" | base64 -w0 | tr -d '=')
printf 'ss://%s@%s:%s#E2E\n' "$userinfo" "$node_ip" "$node_port" | base64 -w0 >"$work/panel-body"
python3 "$root/.github/e2e/panel.py" --port "$panel_port" --body "$work/panel-body" \
  --requests "$work/panel-requests.log" >"$work/panel.log" 2>&1 &
pids+=($!)
wait_for "сайт" curl -fsS "http://$site_ip:$site_port/index.html"
wait_for "панель" curl -fsS "http://127.0.0.1:$panel_port/sub/probe"

cat >"$work/config.toml" <<EOF
[[subscription]]
name = "e2e"
url = "http://127.0.0.1:$panel_port/sub/e2etoken1234"
allow_http = true
app = "happ"
platform = "windows"

[mode]
type = "proxy"
listen = "$proxy"
EOF
sudo install -m 600 "$work/config.toml" /etc/raycat/config.toml

echo "== боевой запуск: подписка, xray и прокси под песочницей"
sudo systemctl start raycat
wait_for "прокси отвечает" site_ok
wait_for "узел выбран" status_is '.node.id == "e2e/E2E"'
pid=$(main_pid)
[ "$(caps_of "$pid")" = "$caps_net_admin" ] || fail "у демона привилегии $(caps_of "$pid")"
xpid=$(xray_pid)
[ -n "$xpid" ] || fail "xray не запущен"
[ "$(caps_of "$xpid")" = "$caps_net_admin" ] || fail "у xray привилегии $(caps_of "$xpid")"
[ "$(sudo stat -c %a /run/raycat/raycat.sock)" = 600 ] || fail "права сокета не 0600"
[ "$(sudo stat -c %a /var/lib/raycat)" = 700 ] || fail "права каталога состояния не 0700"
sudo raycat check >/dev/null || fail "raycat check под root не прошла"

echo "== повторный запуск установщика не перезапускает службу"
out=$(install_from "$release1") || fail "повторная установка не удалась: $out"
grep -q 'перезапускать не нужно' <<<"$out" || fail "нет сообщения, что перезапуск не нужен: $out"
[ "$(main_pid)" = "$pid" ] || fail "служба перезапущена без изменений"

echo "== обновление перезапускает службу"
out=$(install_from "$release2") || fail "обновление не удалось: $out"
grep -q 'Служба raycat запущена' <<<"$out" || fail "после обновления нет сообщения о запуске: $out"
updated=$(main_pid)
[ "$updated" != "$pid" ] || fail "служба не перезапущена после обновления"
wait_for "прокси после обновления" site_ok

echo "== после падения служба поднимается сама"
sudo kill -KILL "$updated"
wait_for "новый процесс демона" restarted "$updated"
wait_for "прокси после падения" site_ok

echo "== остановка снимает xray и каталог сокета"
sudo systemctl stop raycat
if [ -n "$(xray_pid)" ]; then
  fail "xray остался после остановки службы"
fi
[ ! -e /run/raycat ] || fail "каталог /run/raycat остался после остановки"

echo "== удаление при работающей службе"
sudo systemctl start raycat
wait_for "прокси перед удалением" site_ok
out=$(install_from "$release1" --uninstall) || fail "удаление не удалось: $out"
if systemctl is-active --quiet raycat; then fail "служба работает после удаления"; fi
if systemctl is-enabled --quiet raycat 2>/dev/null; then fail "служба включена после удаления"; fi
for path in /etc/systemd/system/raycat.service /usr/local/bin/raycat /usr/libexec/raycat/xray; do
  [ ! -e "$path" ] || fail "после удаления остался $path"
done
sudo test -f /etc/raycat/config.toml || fail "удаление убрало настройки"
sudo test -d /var/lib/raycat || fail "удаление убрало состояние"
out=$(install_from "$release1" --uninstall --purge) || fail "--purge не удался: $out"
[ ! -e /etc/raycat ] || fail "--purge оставил /etc/raycat"
[ ! -e /var/lib/raycat ] || fail "--purge оставил /var/lib/raycat"

echo "Все проверки службы пройдены"
