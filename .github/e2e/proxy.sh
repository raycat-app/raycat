#!/usr/bin/env bash
# e2e режима прокси: панель -> raycat -> xray -> узел (shadowsocks) -> сайт.
#
#   cargo build && XRAY=/путь/к/xray bash .github/e2e/proxy.sh
#
# Узел и сайт слушают публичные адреса, добавленные на lo. Loopback не годится:
# raycat считает узлы на 127.0.0.0/8 заглушками панелей, а трафик к 127.0.0.0/8 xray
# отправляет напрямую, мимо узла. Зарезервированные диапазоны (192.0.2.0/24 и др.)
# тоже не годятся: freedom в xray блокирует их ("blocked target"), поэтому узел не
# смог бы дойти до сайта. Нужен sudo для `ip addr add`.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
raycat=${RAYCAT:-$root/target/debug/raycat}
xray=${XRAY:-$root/xray}

node_ip=11.11.11.10
node_port=18388
site_ip=11.11.11.20
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
    for file in panel-requests.log node-access.log state/xray.json; do
      echo "::group::$file"
      cat "$work/$file" 2>/dev/null
      echo "::endgroup::"
    done
  fi
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

log_has() { grep -q -- "$1" "$work/raycat.log"; }
# Строки лога после первых $1 строк.
log_since() { grep -q -- "$2" <(tail -n "+$(($1 + 1))" "$work/raycat.log"); }
log_lines() { wc -l <"$work/raycat.log"; }

via_http() { curl -fsS --max-time 5 -x "http://$proxy" "http://$site_ip:$site_port/index.html"; }
via_socks() { curl -fsS --max-time 5 --socks5-hostname "$proxy" "http://$site_ip:$site_port/index.html"; }

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

api() { curl -fsS --max-time 20 --unix-socket "$RAYCAT_SOCKET" "http://localhost$1"; }
api_code() {
  local method=$1 path=$2 body=${3:-}
  curl -sS --max-time 30 -o "$work/api-body" -w '%{http_code}' --unix-socket "$RAYCAT_SOCKET" \
    -X "$method" -d "$body" "http://localhost$path"
}
status_is() { api /v1/status | jq -e "$1" >/dev/null; }
nodes_are() { api /v1/nodes | jq -e "$1" >/dev/null; }

"$xray" version
sudo ip addr add "$node_ip/32" dev lo
sudo ip addr add "$site_ip/32" dev lo

echo "== узел, сайт и панель"
cat >"$work/node.json" <<EOF
{
  "log": {"loglevel": "info", "access": "$work/node-access.log"},
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
export RAYCAT_SOCKET="$work/raycat.sock"
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

echo "== API демона по unix-сокету"
[ "$(stat -c %a "$RAYCAT_SOCKET")" = 600 ] || fail "права сокета API не 0600"
wait_for "узел выбран" status_is '.node.id == "e2e/E2E"'
status_is '.mode == "proxy" and .kill_switch == null and .xray.running == true and (.xray.pid | type) == "number"' \
  || fail "status: режим или xray"
status_is '.subscriptions[0].name == "e2e" and .subscriptions[0].title == "E2E" and .subscriptions[0].total_bytes == 1073741824' \
  || fail "status: сведения подписки"
status_is '.subscriptions[0].url | (contains("…1234") and (contains("e2etoken") | not))' \
  || fail "status: ссылка подписки не замаскирована"
status_is '.node.reason | type == "string"' || fail "status: нет причины выбора узла"
expect_site http
wait_for "трафик узла в /v1/nodes" nodes_are '.nodes[0].uplink_bytes | type == "number"'
nodes_are '.nodes | length == 1 and .[0].id == "e2e/E2E" and .[0].selected == true and .[0].pinned == false' \
  || fail "nodes: список узлов"

echo "  закрепление"
[ "$(api_code POST /v1/pin '{"node":"e2e/Nope"}')" = 404 ] || fail "pin неизвестного узла должен давать 404"
jq -e '.error | contains("нет среди")' "$work/api-body" >/dev/null || fail "ошибка pin не на русском"
[ "$(api_code POST /v1/pin 'не json')" = 400 ] || fail "pin с мусором должен давать 400"
[ "$(api_code POST /v1/pin '{"node":"e2e/E2E"}')" = 200 ] || fail "pin не принят"
wait_for "узел закреплён" status_is '.node.pinned == true'
nodes_are '.nodes[0].pinned == true' || fail "nodes: узел не помечен закреплённым"
expect_site http
[ "$(api_code DELETE /v1/pin)" = 200 ] || fail "снятие закрепления не принято"
wait_for "закрепление снято" status_is '.node.pinned == false'

echo "  обновление и события"
requests_before=$(grep -c '^GET /sub' "$work/panel-requests.log")
curl -sN --max-time 20 --unix-socket "$RAYCAT_SOCKET" http://localhost/v1/events >"$work/events.txt" 2>&1 &
events_pid=$!
wait_for "поток событий открыт" grep -q 'event: hello' "$work/events.txt"
[ "$(api_code POST /v1/update '{}')" = 200 ] || fail "update не выполнен"
jq -e '.results[0].subscription == "e2e" and .results[0].ok == true and .results[0].nodes == 1' "$work/api-body" >/dev/null \
  || fail "update: результат"
[ "$(grep -c '^GET /sub' "$work/panel-requests.log")" -gt "$requests_before" ] || fail "update не дошёл до панели"
wait_for "событие обновления подписки" grep -q 'event: subscription_updated' "$work/events.txt"
[ "$(api_code POST /v1/update '{"subscription":"nope"}')" = 404 ] || fail "update неизвестной подписки должен давать 404"
[ "$(api_code POST /v1/pin '{"node":"e2e/E2E"}')" = 200 ] || fail "pin не принят"
wait_for "событие закрепления" grep -q 'event: pin' "$work/events.txt"
kill "$events_pid" 2>/dev/null || true
wait "$events_pid" 2>/dev/null || true
[ "$(api_code GET /v1/nothing)" = 404 ] || fail "неизвестный путь должен давать 404"

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
[ ! -e "$RAYCAT_SOCKET" ] || fail "сокет API остался после остановки"
[ -s "$work/state/pin.json" ] || fail "закрепление не сохранено в каталоге состояния"

echo "== перезапуск демона при недоступной панели: прокси из кэша, закрепление на месте"
mark=$(log_lines)
start_daemon
wait_for "прокси отвечает из кэша" via_http
expect_site http
expect_site socks
log_since "$mark" "кэш от" || fail "в логе нет строки о кэше"
wait_for "ошибка обновления в логе" log_since "$mark" "не удалось обновить"
wait_for "закрепление пережило перезапуск" status_is '.node.pinned == true and .node.id == "e2e/E2E"'
[ "$(api_code DELETE /v1/pin)" = 200 ] || fail "снятие закрепления не принято"
wait_for "закрепление снято" status_is '.node.pinned == false'
status_is '.subscriptions[0].last_error | type == "string"' || fail "status: нет ошибки обновления подписки"
stop_daemon

echo "e2e прокси: успех"
