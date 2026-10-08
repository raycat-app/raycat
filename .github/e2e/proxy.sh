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
site_tls_port=18443
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
    for log in raycat node panel site tls-site; do
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

expect_refused() {
  local how=$1
  shift
  if curl -fsS --max-time 5 "$@" "http://$site_ip:$site_port/index.html" >/dev/null 2>&1; then
    fail "прокси пустил без верного пароля ($how)"
  fi
}

expect_authorized() {
  local how=$1 body
  shift
  body=$(curl -fsS --max-time 5 "$@" "http://$site_ip:$site_port/index.html") || fail "прокси с паролем ($how) не отвечает"
  [ "$body" = "raycat-e2e-ok" ] || fail "прокси с паролем ($how) вернул чужой ответ: $body"
}

start_daemon() {
  "$raycat" daemon --config "${1:-$work/config.toml}" >>"$work/raycat.log" 2>&1 &
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

status_is() { "$raycat" status --json | jq -e "$1" >/dev/null; }
nodes_are() { "$raycat" nodes --json | jq -e "$1" >/dev/null; }
# Вывод команды (без цветов: stdout не терминал) должен содержать текст $1.
shows() {
  local text=$1 out
  shift
  out=$("$raycat" "$@") || fail "raycat $* завершилась ошибкой"
  grep -q -- "$text" <<<"$out" || fail "raycat $*: в выводе нет «$text»: $out"
}
# Команда должна завершиться ошибкой, а её сообщение содержать текст $1.
refuses() {
  local text=$1
  shift
  if "$raycat" "$@" >"$work/refused.log" 2>&1; then
    fail "raycat $* должна завершиться ошибкой"
  fi
  grep -q -- "$text" "$work/refused.log" || fail "raycat $*: в сообщении нет «$text»: $(cat "$work/refused.log")"
}

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

head -c 1000000 /dev/zero >"$work/site/speed.bin"
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$work/ca.key" -out "$work/ca.crt" \
  -days 1 -subj "/CN=raycat e2e CA" 2>/dev/null
openssl req -newkey rsa:2048 -nodes -keyout "$work/site.key" -out "$work/site.csr" \
  -subj "/CN=$site_ip" 2>/dev/null
printf 'subjectAltName=IP:%s\nbasicConstraints=CA:FALSE\n' "$site_ip" >"$work/site.ext"
openssl x509 -req -in "$work/site.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
  -CAcreateserial -out "$work/site.crt" -days 1 -extfile "$work/site.ext" 2>/dev/null
python3 "$root/.github/e2e/tls_site.py" --bind "$site_ip" --port "$site_tls_port" \
  --directory "$work/site" --cert "$work/site.crt" --key "$work/site.key" >"$work/tls-site.log" 2>&1 &
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
export SSL_CERT_FILE="$work/ca.crt"
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

echo "  человекочитаемый вывод"
shows 'raycat ' status
shows 'режим: *прокси' status
shows 'подписка: *e2e' status
shows 'узлов: *1' status
shows '«E2E»' status
shows 'причина:' status
if "$raycat" status | grep -q 'e2etoken'; then fail "status показал токен подписки"; fi
shows 'E2E' nodes
shows '▶' nodes
if "$raycat" status | grep -q "$(printf '\033')"; then fail "вывод без терминала содержит цвета"; fi

echo "  закрепление"
refuses 'нет среди узлов подписок' use Nope
refuses 'нет среди узлов подписок' use e2e/Nope
use_code=0
"$raycat" use --nope >/dev/null 2>&1 || use_code=$?
[ "$use_code" -eq 2 ] || fail "ошибка использования должна давать код 2, а не $use_code"
"$raycat" use e2e >"$work/use.log" || fail "use по части имени не принят"
grep -q 'закреплён' "$work/use.log" || fail "use не сообщил о закреплении: $(cat "$work/use.log")"
wait_for "узел закреплён" status_is '.node.pinned == true'
nodes_are '.nodes[0].pinned == true' || fail "nodes: узел не помечен закреплённым"
shows '★' nodes
shows 'закреплён вручную' status
"$raycat" use e2e/E2E --json | jq -e '.node == "e2e/E2E"' >/dev/null || fail "use --json: ответ"
expect_site http
"$raycat" use auto >"$work/use.log" || fail "use auto не принят"
grep -q 'Закрепление снято' "$work/use.log" || fail "use auto не сообщил о снятии: $(cat "$work/use.log")"
wait_for "закрепление снято" status_is '.node.pinned == false'

echo "  тест скорости"
wait_for "сайт с TLS" curl -fsS --cacert "$work/ca.crt" "https://$site_ip:$site_tls_port/speed.bin" -o /dev/null
"$raycat" speedtest --url "https://$site_ip:$site_tls_port/speed.bin" --size 1MB --json >"$work/speed.json" \
  || fail "тест скорости не выполнен"
jq -e '(.runs | length) == 2 and all(.runs[]; .bytes == 1000000 and .mbps > 0) and .node == "e2e/E2E"' \
  "$work/speed.json" >/dev/null || fail "тест скорости: результат $(cat "$work/speed.json")"
wait_for "трафик теста прошёл через узел" grep -q "$site_ip:$site_tls_port" "$work/node-access.log"
status_is '.node.pinned == false' || fail "тест скорости оставил закрепление"
"$raycat" use e2e/E2E >/dev/null || fail "use перед тестом с узлом не принят"
wait_for "закрепление перед тестом" status_is '.node.pinned == true'
"$raycat" speedtest e2e/E2E --url "https://$site_ip:$site_tls_port/speed.bin" --size 1MB --streams 2 --json \
  >"$work/speed.json" || fail "тест скорости с узлом не выполнен"
jq -e '(.runs | length) == 1 and .runs[0].streams == 2 and .node == "e2e/E2E"' "$work/speed.json" >/dev/null \
  || fail "тест скорости с узлом: результат $(cat "$work/speed.json")"
status_is '.node.pinned == true and .node.id == "e2e/E2E"' || fail "тест скорости не вернул закрепление"
"$raycat" use auto >/dev/null || fail "use auto после теста не принят"
wait_for "закрепление снято после теста" status_is '.node.pinned == false'

echo "  обновление и события"
requests_before=$(grep -c '^GET /sub' "$work/panel-requests.log")
curl -sN --max-time 20 --unix-socket "$RAYCAT_SOCKET" http://localhost/v1/events >"$work/events.txt" 2>&1 &
events_pid=$!
"$raycat" events >"$work/cli-events.txt" 2>&1 &
cli_events_pid=$!
wait_for "поток событий открыт" grep -q 'event: hello' "$work/events.txt"
wait_for "raycat events подключился" grep -q 'подключено к демону' "$work/cli-events.txt"
"$raycat" update --json >"$work/update.json" || fail "update не выполнен"
jq -e '.results[0].subscription == "e2e" and .results[0].ok == true and .results[0].nodes == 1' "$work/update.json" >/dev/null \
  || fail "update --json: результат"
[ "$(grep -c '^GET /sub' "$work/panel-requests.log")" -gt "$requests_before" ] || fail "update не дошёл до панели"
wait_for "событие обновления подписки" grep -q 'event: subscription_updated' "$work/events.txt"
wait_for "raycat events показал обновление" grep -q 'подписка «e2e» обновлена' "$work/cli-events.txt"
"$raycat" update e2e >"$work/update.log" || fail "update по имени не выполнен"
grep -q '✓ e2e: узлов: 1' "$work/update.log" || fail "update: нет итога по подписке: $(cat "$work/update.log")"
refuses 'nope' update nope
"$raycat" use e2e/E2E >/dev/null || fail "use не принят"
wait_for "событие закрепления" grep -q 'event: pin' "$work/events.txt"
wait_for "raycat events показал закрепление" grep -q 'закреплён узел e2e/E2E' "$work/cli-events.txt"
kill "$events_pid" "$cli_events_pid" 2>/dev/null || true
wait "$events_pid" "$cli_events_pid" 2>/dev/null || true

echo "  завершение, автодополнение, man"
"$raycat" completions bash | grep -q 'raycat' || fail "completions bash пусто"
"$raycat" completions zsh | grep -q 'raycat' || fail "completions zsh пусто"
"$raycat" completions fish | grep -q 'raycat' || fail "completions fish пусто"
"$raycat" man | grep -q '^\.TH' || fail "man не roff"

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
"$raycat" use auto >/dev/null || fail "снятие закрепления не принято"
wait_for "закрепление снято" status_is '.node.pinned == false'
status_is '.subscriptions[0].last_error | type == "string"' || fail "status: нет ошибки обновления подписки"
stop_daemon

echo "== прокси с паролем: без пароля не пускает, с паролем пускает"
auth_proxy=127.0.0.1:7891
auth_user=e2e-user
auth_pass=e2e-proxy-pass
cat >"$work/auth.toml" <<EOF
[[subscription]]
name = "e2e"
url = "http://127.0.0.1:$panel_port/sub/e2etoken1234"
allow_http = true
app = "happ"
platform = "windows"

[mode]
type = "proxy"
listen = "$auth_proxy"
auth = "$auth_user:$auth_pass"

[xray]
path = "$xray"

[log]
level = "debug"
EOF
start_daemon "$work/auth.toml"
wait_for "прокси с паролем отвечает" curl -fsS --max-time 5 -x "http://$auth_user:$auth_pass@$auth_proxy" "http://$site_ip:$site_port/index.html"
expect_refused http -x "http://$auth_proxy"
expect_refused socks -x "socks5h://$auth_proxy"
expect_refused "неверный пароль" -x "http://$auth_user:wrong-$auth_pass@$auth_proxy"
expect_authorized http -x "http://$auth_user:$auth_pass@$auth_proxy"
expect_authorized socks -x "socks5h://$auth_user:$auth_pass@$auth_proxy"
if grep -q -- "$auth_pass" "$work/raycat.log"; then fail "пароль прокси попал в лог демона"; fi
stop_daemon

"$raycat" check --config "$work/auth.toml" >"$work/auth-check.log" 2>&1 || fail "check с паролем завершился ошибкой: $(cat "$work/auth-check.log")"
grep -q 'вход по логину и паролю' "$work/auth-check.log" || fail "check не сообщил о входе по паролю"
if grep -q -- "$auth_pass" "$work/auth-check.log"; then fail "check показал пароль прокси"; fi

echo "e2e прокси: успех"
