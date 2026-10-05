#!/usr/bin/env bash
# e2e шлюза для локальной сети: устройство в сети lan ходит в интернет только через
# узел, а хост-маршрутизатор с raycat при этом остаётся доступным: соединения к нему
# (ssh-подобные) не рвутся ни при запуске, ни при падении xray, ни при остановке.
#
#   docker load < образ   # тег raycat:ci
#   bash .github/e2e/lan.sh
#
# Стенд описан в .github/e2e/lan/compose.yml. Нужен Docker с compose v2 и python3.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
image=${RAYCAT_IMAGE:-raycat:ci}
state=$(mktemp -d)
chmod 777 "$state"
conf=$state/conf
mkdir "$conf"
export LAN_STATE=$state
compose=(docker compose -f "$here/lan/compose.yml")

router=raycat-e2e-lan-router
client=raycat-e2e-lan-client
outsider=raycat-e2e-lan-outsider
watcher=raycat-e2e-lan-watcher
daemon=raycat-e2e-lan-daemon
site=http://site.e2e.test/
canary=http://11.60.0.50/
nodes_inet=(11.70.0.11 11.70.0.12)
nodes_wan=(11.60.0.11 11.60.0.12)

fail() {
  echo "ПРОВАЛ: $*" >&2
  exit 1
}

cleanup() {
  local status=$?
  set +e
  if [ "$status" -ne 0 ]; then
    docker ps -a --format 'table {{.Names}}\t{{.Status}}' >&2
    echo "::group::журнал $daemon"
    docker logs "$daemon" 2>&1 | tail -80
    echo "::endgroup::"
    echo "::group::правила маршрутизатора"
    docker exec "$router" nft list ruleset 2>&1
    docker exec "$router" ip -4 rule show 2>&1
    docker exec "$router" ip -4 route show table all 2>&1
    echo "::endgroup::"
    echo "::group::сеть клиента"
    docker exec "$client" ip addr 2>&1
    docker exec "$client" ip route 2>&1
    docker exec "$client" cat /etc/resolv.conf 2>&1
    echo "::endgroup::"
    echo "::group::состояние соединений к хосту"
    tail -n 3 "$state"/*.state 2>&1
    echo "::endgroup::"
  fi
  docker rm -f "$daemon" >/dev/null 2>&1
  "${compose[@]}" down --volumes --remove-orphans --timeout 3 >/dev/null 2>&1
  rm -rf "$state"
  exit "$status"
}
trap cleanup EXIT

client_run() { docker exec "$client" "$@"; }
router_run() { docker exec "$router" "$@"; }
fetch() { client_run wget -q -T 5 -O - "$1"; }

wait_for() {
  local what=$1 tries=$2
  shift 2
  local _
  for _ in $(seq "$tries"); do
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  fail "не дождались: $what"
}

in_list() {
  local needle=$1
  shift
  local item
  for item in "$@"; do
    [ "$item" = "$needle" ] && return 0
  done
  return 1
}

# Подстановка процесса, а не конвейер: ранний выход grep не должен ронять pipefail.
log_has() { grep -q -- "$2" <(docker logs "$1" 2>&1); }
rule_present() { grep -q 7263 <(router_run ip -4 rule show); }
tables_present() { router_run nft list table inet raycat; }

# Ответ сайта — адрес, с которого к нему пришли.
expect_via_node() {
  local url=$1
  shift
  local seen
  seen=$(fetch "$url") || fail "$url не отвечает у устройства сети"
  in_list "$seen" "$@" || fail "$url ответил адресом $seen, ожидался адрес узла: $*"
  echo "  $url: пришли с адреса узла $seen"
}

via_node() {
  local seen
  seen=$(fetch "$1") || return 1
  in_list "$seen" "${nodes_wan[@]}"
}

expect_seen_as() {
  local url=$1 want=$2 seen
  seen=$(fetch "$url") || fail "$url не отвечает у устройства сети"
  [ "$seen" = "$want" ] || fail "$url ответил «$seen», ожидалось «$want»"
  echo "  $url: пришли с адреса $seen"
}

set_forward() {
  docker run --rm --privileged --network "container:$router" --entrypoint sysctl "$image" \
    -w "net.ipv4.ip_forward=$1" >/dev/null
}
get_forward() { router_run cat /proc/sys/net/ipv4/ip_forward; }

# Соединения к хосту: «из интернета» (outsider) и «из сети» (watcher) к службе на 2222.
open_stream() {
  local container=$1 address=$2 name=$3 _
  docker exec --detach "$container" python /e2e/stream.py watch "$address" 2222 --state "/state/$name.state"
  for _ in $(seq 40); do
    if grep -q '^ok' "$state/$name.state" 2>/dev/null; then
      return 0
    fi
    sleep 0.25
  done
  fail "не дождались соединения $name"
}

alive() {
  local what=$1 name out
  shift
  for name in "$@"; do
    out=$(python3 "$here/stream.py" check --state "$state/$name.state" 2>&1) ||
      fail "соединение $name не пережило: $what ($out)"
  done
  echo "  $what: соединения $* живы"
}

write_config() {
  cat >"$conf/$1.toml" <<EOF
[[subscription]]
name = "e2e"
url = "http://11.60.0.5:8080/sub/e2etoken1234"
allow_http = true
app = "happ"
platform = "windows"

[mode]
type = "gateway"
kill_switch = true
lan = true
$2

[log]
level = "debug"
EOF
}

run_check() {
  docker run --rm --network "container:$router" --env "RAYCAT_CONFIG=/lan/$1.toml" \
    --volume "$conf:/lan:ro" "$image" check 2>&1 || true
}

start_daemon() {
  docker run --detach --name "$daemon" --network "container:$router" --cap-add NET_ADMIN \
    --env RAYCAT_CONFIG=/lan/main.toml --volume "$conf:/lan:ro" "$image" >/dev/null
}

echo "== образ и стенд"
docker image inspect "$image" >/dev/null || fail "нет образа $image"
[ "$image" = raycat:ci ] || docker tag "$image" raycat:ci
"${compose[@]}" up --detach --wait --wait-timeout 180 >/dev/null
docker ps --format 'table {{.Names}}\t{{.Status}}'

lan_if=$(router_run ip -o -4 addr show | awk '$4 ~ /^10\.77\.0\.2\// { print $2 }')
wan_if=$(router_run ip -o -4 addr show | awk '$4 ~ /^11\.60\.0\.2\// { print $2 }')
[ -n "$lan_if" ] && [ -n "$wan_if" ] || fail "не нашли интерфейсы маршрутизатора (lan: $lan_if, wan: $wan_if)"
echo "  интерфейсы маршрутизатора: сеть $lan_if, интернет $wan_if"
write_config main "lan_interface = \"$lan_if\""
write_config auto ""
write_config bad "lan_interface = \"nope0\""

client_run ip route replace default via 10.77.0.2
client_run sh -c 'echo "nameserver 10.77.0.2" > /etc/resolv.conf'

echo "== стенд: прежний маршрутизатор с NAT выпускает устройство наружу напрямую"
set_forward 1
router_run nft add table ip legacy
router_run nft add chain ip legacy post '{ type nat hook postrouting priority 100; }'
router_run nft add rule ip legacy post oifname "$wan_if" masquerade
expect_seen_as "$canary" 11.60.0.2
client_run ping -c 1 -W 2 11.60.0.50 >/dev/null || fail "стенд не пересылает ICMP"
set_forward 0

echo "== соединения к хосту открываются до запуска raycat"
open_stream "$outsider" 11.60.0.2 wan
open_stream "$watcher" 10.77.0.2 lan
alive "до запуска raycat" wan lan

echo "== настройки: интерфейс и подсети определяются"
out=$(run_check main)
echo "$out"
grep -q "интерфейс $lan_if, перехватываются устройства из подсетей 10.77.0.0/24" <<<"$out" ||
  fail "check не показал интерфейс и подсеть устройств"
out=$(run_check auto)
echo "$out"
grep -q "интерфейс $wan_if, перехватываются устройства из подсетей 11.60.0.0/24" <<<"$out" ||
  fail "без lan_interface не выбран интерфейс маршрута по умолчанию"
out=$(run_check bad)
echo "$out"
grep -q "нет в системе" <<<"$out" || fail "несуществующий интерфейс не замечен"

echo "== raycat запущен: устройство ходит через узел без ip_forward"
start_daemon
wait_for "правила в маршрутизаторе" 60 tables_present
wait_for "сайт через шлюз" 90 fetch "$site"
[ "$(get_forward)" = 0 ] || fail "демон изменил ip_forward"
log_has "$daemon" "правила перехвата установлены, kill switch включён" || fail "в журнале нет строки об установке правил"
log_has "$daemon" "локальная сеть: интерфейс $lan_if" || fail "в журнале нет строки о сети устройств"
expect_via_node "$site" "${nodes_inet[@]}"
expect_via_node "$canary" "${nodes_wan[@]}"
alive "raycat запущен" wan lan
open_stream "$outsider" 11.60.0.2 wan-new
alive "новое входящее соединение при правилах" wan-new

echo "== DNS устройства отвечает fake-IP: и хост, и любой другой сервер"
answer=$(client_run nslookup site.e2e.test) || fail "nslookup к хосту не ответил"
echo "$answer" | grep -q 'Address: 198\.1[89]\.' || fail "DNS хоста ответил не fake-IP: $answer"
outside=$(client_run nslookup site.e2e.test 1.1.1.1) || fail "nslookup к 1.1.1.1 не ответил"
echo "$outside" | grep -q 'Address: 198\.1[89]\.' || fail "DNS к внешнему серверу ответил не fake-IP: $outside"

echo "== хост и соседи сети не перехватываются"
expect_seen_as http://10.77.0.2:8081/ host-10.77.0.10
expect_seen_as http://10.77.0.20/ 10.77.0.10

echo "== ip_forward=1 и NAT на хосте: наружу напрямую устройство не выходит"
set_forward 1
expect_via_node "$canary" "${nodes_wan[@]}"
if client_run ping -c 1 -W 2 11.60.0.50 >/dev/null 2>&1; then
  fail "ICMP устройства вышел наружу напрямую"
fi
if client_run wget -q -T 4 -O - http://10.77.0.2:12345/ >/dev/null 2>&1; then
  fail "устройство дошло до порта xray напрямую"
fi
alive "пересылка включена" wan lan wan-new

echo "== kill switch: xray убит, демон заморожен"
docker kill --signal STOP "$daemon" >/dev/null
docker exec "$daemon" sh -c 'kill -9 $(pidof xray)'
sleep 1
if leaked=$(client_run wget -q -T 4 -O - "$canary" 2>&1); then
  fail "трафик устройства ушёл мимо xray: $leaked"
fi
if leaked=$(client_run wget -q -T 4 -O - "$site" 2>&1); then
  fail "трафик по имени прошёл без xray: $leaked"
fi
if client_run timeout 8 nslookup site.e2e.test 1.1.1.1 >/dev/null 2>&1; then
  fail "DNS-запрос устройства вышел наружу без xray"
fi
if client_run timeout 8 nslookup site.e2e.test >/dev/null 2>&1; then
  fail "DNS-запрос к хосту сработал без xray"
fi
if client_run ping -c 1 -W 2 11.60.0.50 >/dev/null 2>&1; then
  fail "ICMP устройства вышел наружу без xray"
fi
echo "  без xray у устройства нет ни прямого выхода, ни DNS"
expect_seen_as http://10.77.0.2:8081/ host-10.77.0.10
alive "xray убит" wan lan wan-new
docker kill --signal CONT "$daemon" >/dev/null
wait_for "xray перезапущен и трафик снова идёт" 90 via_node "$canary"
log_has "$daemon" "xray завершился" || fail "в журнале нет записи о падении xray"
alive "xray перезапущен" wan lan wan-new

echo "== штатная остановка снимает правила: хост как был"
docker stop "$daemon" >/dev/null
code=$(docker inspect -f '{{.State.ExitCode}}' "$daemon")
[ "$code" = 0 ] || fail "демон завершился с кодом $code после docker stop"
log_has "$daemon" "правила перехвата сняты" || fail "в журнале нет строки о снятии правил"
if tables_present >/dev/null 2>&1; then
  fail "таблица nftables осталась после штатной остановки"
fi
if rule_present; then
  fail "правило маршрутизации осталось после штатной остановки"
fi
[ "$(get_forward)" = 1 ] || fail "после остановки изменился ip_forward"
expect_seen_as "$canary" 11.60.0.2
alive "демон остановлен" wan lan wan-new

echo "== повторный запуск возвращает перехват"
docker start "$daemon" >/dev/null
wait_for "трафик через узел после запуска" 90 via_node "$canary"
alive "демон запущен снова" wan lan wan-new

echo "== авария: правила остаются и держат kill switch, следующий запуск ставит их заново"
docker kill --signal KILL "$daemon" >/dev/null
sleep 1
tables_present >/dev/null || fail "после аварии исчезла таблица nftables"
rule_present || fail "после аварии исчезло правило маршрутизации"
if leaked=$(client_run wget -q -T 4 -O - "$canary" 2>&1); then
  fail "после аварии трафик устройства ушёл напрямую: $leaked"
fi
alive "демон убит" wan lan wan-new
docker start "$daemon" >/dev/null
wait_for "трафик через узел после аварии" 90 via_node "$canary"
[ "$(router_run ip -4 rule show | grep -c 7263)" = 1 ] || fail "после перезапуска не одно правило маршрутизации"
alive "демон перезапущен после аварии" wan lan wan-new

echo "e2e шлюза для локальной сети: успех"
