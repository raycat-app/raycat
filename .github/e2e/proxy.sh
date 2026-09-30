#!/usr/bin/env bash
# e2e режима прокси: панель -> raycat -> xray -> узел (shadowsocks) -> сайт.
#
#   cargo build && XRAY=/путь/к/xray bash .github/e2e/proxy.sh
#
# Узел и сайт слушают адреса из 192.0.2.0/24, добавленные на lo: raycat не считает
# узлами адреса 127.0.0.0/8 (это заглушки панелей), а трафик к 127.0.0.0/8 xray
# отправляет напрямую, мимо узла. Нужен sudo для `ip addr add`.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
raycat=${RAYCAT:-$root/target/debug/raycat}
xray=${XRAY:-$root/xray}

node_ip=192.0.2.10
node_port=18388
site_ip=192.0.2.20
site_port=18080
panel_port=18090
proxy=127.0.0.1:7890
password=e2e-password

work=$(mktemp -d)
pids=()
daemon_pid=

fail() {
  echo "ПРОВАЛ: $*" >&2
  exit 1
}

cleanup() {
  local status=$?
  set +e
  [ -n "$daemon_pid" ] && kill "$daemon_pid" 2>/dev/null
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null; done
  wait 2>/dev/null
  if [ "$status" -ne 0 ]; then
    for log in raycat node panel site; do
      echo "::group::$log.log"
      cat "$work/$log.log" 2>/dev/null
      echo "::endgroup::"
    done
    echo "::group::panel-requests.log"
    cat "$work/panel-requests.log" 2>/dev/null
    echo "::endgroup::"
  fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT

wait_for() {
  local what=$1
  shift
  local _
  for _ in $(seq 60); do
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  fail "не дождались: $what"
}

log_has() { grep -q -- "$1" "$work/raycat.log"; }
# Строки лога после первых $1 строк.
log_since() { grep -q -- "$2" <(tail -n "+$(($1 + 1))" "$work/raycat.log"); }
log_lines() { wc -l <"$work/raycat.log"; }

via_http() { curl -fsS --max-time 15 -x "http://$proxy" "http://$site_ip:$site_port/index.html"; }
via_socks() { curl -fsS --max-time 15 --socks5-hostname "$proxy" "http://$site_ip:$site_port/index.html"; }

expect_site() {
  local how=$1 body
  body=$("via_$how") || fail "прокси ($how) не отвечает"
  [ "$body" = "raycat-e2e-ok" ] || fail "прокси ($how) вернул чужой ответ: $body"
}

start_daemon() {
  "$raycat" daemon --config "$work/config.toml" >>"$work/raycat.log" 2>&1 &
  daemon_pid=$!
}

stop_daemon() {
  local code=0
  kill -TERM "$daemon_pid"
  wait "$daemon_pid" || code=$?
  daemon_pid=
  [ "$code" -eq 0 ] || fail "демон завершился с кодом $code, ожидался 0"
  if pgrep -f "$work/state/xray.json" >/dev/null; then
    fail "xray остался работать после остановки демона"
  fi
}

"$xray" version
sudo ip addr add "$node_ip/32" dev lo
sudo ip addr add "$site_ip/32" dev lo

echo "== узел, сайт и панель"
cat >"$work/node.json" <<EOF
{
  "log": {"loglevel": "warning", "access": "$work/node-access.log"},
  "inbounds": [{
    "listen": "$node_ip",
    "port": $node_port,
    "protocol": "shadowsocks",
    "settings": {"method": "aes-128-gcm", "password": "$password", "network": "tcp,udp"}
  }],
  "outbounds": [{"protocol": "freedom"}]
}
EOF
"$xray" run -test -c "$work/node.json"
"$xray" run -c "$work/node.json" >"$work/node.log" 2>&1 &
pids+=($!)

mkdir "$work/site"
echo raycat-e2e-ok >"$work/site/index.html"
python3 -m http.server "$site_port" --bind "$site_ip" --directory "$work/site" >"$work/site.log" 2>&1 &
pids+=($!)

userinfo=$(printf 'aes-128-gcm:%s' "$password" | base64 -w0 | tr -d '=')
printf 'ss://%s@%s:%s#E2E\n' "$userinfo" "$node_ip" "$node_port" | base64 -w0 >"$work/panel-body"
python3 "$root/.github/e2e/panel.py" --port "$panel_port" --body "$work/panel-body" \
  --requests "$work/panel-requests.log" >"$work/panel.log" 2>&1 &
panel_pid=$!
pids+=("$panel_pid")

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

[xray]
path = "$xray"

[log]
level = "debug"
EOF
export RAYCAT_STATE_DIR="$work/state"
: >"$work/raycat.log"
: >"$work/panel-requests.log"

echo "== raycat check без кэша сообщает об этом"
if "$raycat" check --config "$work/config.toml" >"$work/check.log" 2>&1; then
  fail "check без кэша должен завершиться ошибкой"
fi
grep -q "кэша подписок нет" "$work/check.log" || fail "check не сообщил об отсутствии кэша"

echo "== старт: подписка, xray, прокси"
start_daemon
wait_for "прокси отвечает" via_http
expect_site http
expect_site socks
log_has "подписка «e2e» обновлена" || fail "в логе нет строки об обновлении подписки"
wait_for "трафик дошёл до узла" grep -q "$site_ip:$site_port" "$work/node-access.log"
grep -qi '^user-agent: Happ/' "$work/panel-requests.log" || fail "панель не получила User-Agent Happ"
grep -qi '^x-hwid: ' "$work/panel-requests.log" || fail "панель не получила X-Hwid"

echo "== raycat check с кэшем"
"$raycat" check --config "$work/config.toml" || fail "check с кэшем завершился ошибкой"
"$raycat" identity --config "$work/config.toml" | grep -q 'HWID: ' || fail "identity не показал HWID"

echo "== панель остановлена, SIGHUP: демон живёт на кэше"
kill "$panel_pid"
wait "$panel_pid" 2>/dev/null || true
mark=$(log_lines)
kill -HUP "$daemon_pid"
wait_for "ошибка обновления в логе" log_since "$mark" "не удалось обновить"
kill -0 "$daemon_pid" || fail "демон упал после SIGHUP"
expect_site http
[ "$(grep -c 'xray запущен' "$work/raycat.log")" -eq 1 ] || fail "xray перезапускался без причины"

echo "== остановка демона"
stop_daemon

echo "== перезапуск демона при недоступной панели: прокси из кэша"
mark=$(log_lines)
start_daemon
wait_for "прокси отвечает из кэша" via_http
expect_site http
expect_site socks
log_since "$mark" "кэш от" || fail "в логе нет строки о кэше"
wait_for "ошибка обновления в логе" log_since "$mark" "не удалось обновить"
stop_daemon

echo "e2e прокси: успех"
