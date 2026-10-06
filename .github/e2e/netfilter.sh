#!/usr/bin/env bash
# e2e правил перехвата на настоящем ядре. Сначала два сетевых пространства, соединённых
# veth: в «клиенте» стоят правила, «сервер» изображает интернет и приватную сеть.
# Роль xray играет прозрачный сервер netfilter_peer.py. Затем три пространства для
# шлюза локальной сети: устройство, маршрутизатор с правилами и интернет.
#
#   cargo build --example ctl -p raycat-netfilter && bash .github/e2e/netfilter.sh
#
# Нужен sudo (ip netns, nft) и python3. Адреса 203.0.113.0/24 и 2001:db8:1::/64 —
# «публичные» (их правила перехватывают), 10.99.0.0/24 и fd00:99::/64 — приватные.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
ctl=${CTL:-$root/target/debug/examples/ctl}
peer=$root/.github/e2e/netfilter_peer.py
golden=$root/crates/netfilter/tests/golden

client=rcnft-client
server=rcnft-server
lan_client=rcnft-lclient
lan_router=rcnft-lrouter
lan_server=rcnft-lserver
work=$(mktemp -d)

fail() {
  echo "ПРОВАЛ: $*" >&2
  exit 1
}

in_client() { sudo ip netns exec "$client" "$@"; }
in_server() { sudo ip netns exec "$server" "$@"; }
in_lclient() { sudo ip netns exec "$lan_client" "$@"; }
in_lrouter() { sudo ip netns exec "$lan_router" "$@"; }
in_lserver() { sudo ip netns exec "$lan_server" "$@"; }

cleanup() {
  local status=$?
  set +e
  if [ "$status" -ne 0 ]; then
    for file in server.log peers.log lan-server.log lan-peers.log; do
      echo "::group::$file"
      cat "$work/$file" 2>/dev/null
      echo "::endgroup::"
    done
    echo "::group::nft list ruleset (клиент)"
    in_client nft list ruleset 2>&1
    echo "::endgroup::"
    echo "::group::ip rule (клиент)"
    in_client ip -4 rule show 2>&1
    in_client ip -6 rule show 2>&1
    echo "::endgroup::"
    echo "::group::nft list ruleset (маршрутизатор)"
    in_lrouter nft list ruleset 2>&1
    in_lrouter ip -4 rule show 2>&1
    in_lrouter ip -4 route show table all 2>&1
    echo "::endgroup::"
  fi
  for ns in "$client" "$server" "$lan_client" "$lan_router" "$lan_server"; do
    sudo ip netns pids "$ns" 2>/dev/null | xargs -r sudo kill 2>/dev/null
  done
  for ns in "$client" "$server" "$lan_client" "$lan_router" "$lan_server"; do
    sudo ip netns del "$ns" 2>/dev/null
  done
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT

default() { "$ctl" defaults | awk -v key="$1" '$1 == key { print $2 }'; }
own_mark=$(default own_mark)
table=$(default table)
port=$(default port)
test -n "$own_mark" && test -n "$table" && test -n "$port"

sudo ip netns add "$client"
sudo ip netns add "$server"
sudo ip link add rcnft-c type veth peer name rcnft-s
sudo ip link set rcnft-c netns "$client"
sudo ip link set rcnft-s netns "$server"
in_client ip link set lo up
in_server ip link set lo up
in_client ip link set rcnft-c up
in_server ip link set rcnft-s up
in_client ip addr add 203.0.113.2/24 dev rcnft-c
in_client ip addr add 10.99.0.2/24 dev rcnft-c
in_client ip addr add 2001:db8:1::2/64 dev rcnft-c nodad
in_client ip addr add fd00:99::2/64 dev rcnft-c nodad
in_server ip addr add 203.0.113.1/24 dev rcnft-s
in_server ip addr add 10.99.0.1/24 dev rcnft-s
in_server ip addr add 2001:db8:1::1/64 dev rcnft-s nodad
in_server ip addr add fd00:99::1/64 dev rcnft-s nodad

: >"$work/server.log"
: >"$work/peers.log"

rule_count() { in_client ip "$1" rule show | grep -c fwmark || true; }

assert_clean() {
  local vtable=${1:-$table} family
  [ -z "$(in_client nft list ruleset)" ] || fail "после remove остались правила nft"
  for family in -4 -6; do
    [ "$(rule_count "$family")" = 0 ] || fail "после remove остались правила ip rule ($family)"
    [ -z "$(in_client ip "$family" route show table "$vtable" 2>/dev/null || true)" ] ||
      fail "после remove осталась таблица маршрутов $vtable ($family)"
  done
}

echo "== синтаксис эталонных правил (nft -c)"
for file in "$golden"/*.nft; do
  in_client nft -c -f "$file" || fail "nft не принял $(basename "$file")"
done

declare -A variants=(
  [intercept]=""
  [kill-switch]="--kill-switch"
  [ipv6]="--ipv6"
  [ipv6-kill-switch]="--ipv6 --kill-switch"
  [lan]="--lan-interface rcnft-c --lan-subnets 10.99.0.0/24"
  [lan-kill-switch]="--kill-switch --lan-interface rcnft-c --lan-subnets 10.99.0.0/24"
  [custom]="--bypass 192.0.2.0/24,10.0.0.0/8,10.1.0.0/16,2001:db8::/32 --port 7893 --own-mark 0x10000001 --intercept-mark 0x10000002 --table 4711 --priority 4711"
)
for name in "${!variants[@]}"; do
  read -r -a flags <<<"${variants[$name]}"
  vtable=$table
  if [ "$name" = custom ]; then vtable=4711; fi
  echo "== $name: установка, повторная установка, снятие"
  diff -u "$golden/$name.nft" <("$ctl" print "${flags[@]}") ||
    fail "ctl print для $name не совпадает с эталоном"
  in_client "$ctl" install "${flags[@]}"
  in_client nft list table inet raycat >/dev/null || fail "после install нет таблицы ($name)"
  [ "$(rule_count -4)" = 1 ] || fail "после install не одно правило ip rule ($name)"
  in_client "$ctl" install "${flags[@]}"
  [ "$(rule_count -4)" = 1 ] || fail "повторная установка размножила ip rule ($name)"
  in_client "$ctl" remove "${flags[@]}"
  assert_clean "$vtable"
  in_client "$ctl" remove "${flags[@]}"
  assert_clean "$vtable"
done

hits() { wc -l <"$work/server.log" | tr -d ' '; }

# expect_reply КТО ЖДЁМ probe-аргументы: ответ должен содержать строку ЖДЁМ.
expect_reply() {
  local who=$1 want=$2 out
  shift 2
  out=$("$who" python3 "$peer" probe "$@") || fail "нет ответа: $* (ждали «$want»)"
  case $out in
    *"$want"*) ;;
    *) fail "на $* ответили «$out», ждали «$want»" ;;
  esac
}

# no_leak КТО probe-аргументы: ответа нет, и сервер ничего не получил.
no_leak() {
  local who=$1 before out
  shift
  before=$(hits)
  if out=$("$who" python3 "$peer" probe "$@" 2>/dev/null); then
    fail "неожиданный ответ «$out» на $*"
  fi
  [ "$(hits)" = "$before" ] || fail "запрос дошёл до сервера мимо перехвата: $*"
}

# parallel_no_leak ЖУРНАЛ КТО "probe-аргументы"...: то же, что no_leak для нескольких
# запросов, но без ожидания тайм-аута каждого по очереди.
parallel_no_leak() {
  local counter=$1 who=$2 before out spec pid n=0
  shift 2
  local pids=()
  rm -f "$work"/leak.*
  before=$("$counter")
  for spec in "$@"; do
    n=$((n + 1))
    (
      read -r -a probe_args <<<"$spec"
      if out=$("$who" python3 "$peer" probe "${probe_args[@]}" 2>/dev/null); then
        echo "неожиданный ответ «$out» на $spec" >"$work/leak.$n"
      fi
    ) &
    pids+=($!)
  done
  for pid in "${pids[@]}"; do
    wait "$pid"
  done
  if compgen -G "$work/leak.*" >/dev/null; then
    fail "$(cat "$work"/leak.*)"
  fi
  [ "$("$counter")" = "$before" ] || fail "запрос дошёл до сервера мимо перехвата: $*"
}
no_leaks() { parallel_no_leak hits "$@"; }

serve() {
  local who=$1
  shift
  "$who" python3 "$peer" serve "$@" >>"$work/peers.log" 2>&1 &
}

wait_listening() {
  local who=$1 kind=$2 want=$3 _
  for _ in $(seq 40); do
    if "$who" ss -H -ln"$kind" | grep -q ":$want "; then
      return 0
    fi
    sleep 0.25
  done
  fail "не дождались сервера на порту $want"
}

# stop_peers МЕТКА КТО ПОРТ: останавливает прозрачные серверы и ждёт, пока порты освободятся.
stop_peers() {
  local label=$1 who=$2 want=$3 kind _
  sudo pkill -f "[l]abel $label" || true
  for kind in t u; do
    for _ in $(seq 100); do
      if ! "$who" ss -H -ln"$kind" | grep -q ":$want "; then
        break
      fi
      sleep 0.1
    done
    if "$who" ss -H -ln"$kind" | grep -q ":$want "; then
      fail "сервер $label не остановился"
    fi
  done
}

log=(--log "$work/server.log")
serve in_server tcp --bind 203.0.113.1 --port 8080 --label srv "${log[@]}"
serve in_server tcp --bind 10.99.0.1 --port 8080 --label srv "${log[@]}"
serve in_server udp --bind 203.0.113.1 --port 9090 --label srv "${log[@]}"
serve in_server tcp --bind 2001:db8:1::1 --port 8080 --label srv "${log[@]}"
serve in_server tcp --bind fd00:99::1 --port 8080 --label srv "${log[@]}"
serve in_client tcp --bind 203.0.113.2 --port 9000 --label cli
wait_listening in_server t 8080
wait_listening in_server u 9090
wait_listening in_client t 9000

echo "== без правил всё идёт напрямую"
expect_reply in_client "srv 203.0.113.1:8080" tcp 203.0.113.1 8080
expect_reply in_client "srv 203.0.113.1:9090" udp 203.0.113.1 9090
expect_reply in_client "srv 2001:db8:1::1:8080" tcp 2001:db8:1::1 8080
expect_reply in_server "cli 203.0.113.2:9000" tcp 203.0.113.2 9000

echo "== перехват включён, xray не слушает: наружу ничего не уходит"
in_client "$ctl" install
no_leaks in_client "tcp 203.0.113.1 8080" "udp 203.0.113.1 9090" "tcp 2001:db8:1::1 8080"
expect_reply in_client "srv 10.99.0.1:8080" tcp 10.99.0.1 8080
expect_reply in_client "srv fd00:99::1:8080" tcp fd00:99::1 8080
expect_reply in_client "srv 203.0.113.1:8080" tcp 203.0.113.1 8080 --mark "$own_mark"
expect_reply in_server "cli 203.0.113.2:9000" tcp 203.0.113.2 9000

echo "== xray слушает: tcp, udp и DNS попадают в него"
serve in_client tcp --bind 0.0.0.0 --port "$port" --label tproxy --transparent
serve in_client udp --bind 0.0.0.0 --port "$port" --label tproxy --transparent
wait_listening in_client t "$port"
wait_listening in_client u "$port"
intercepted() {
  expect_reply in_client "tproxy 203.0.113.1:8080" tcp 203.0.113.1 8080
  expect_reply in_client "tproxy 203.0.113.1:9090" udp 203.0.113.1 9090
  expect_reply in_client "tproxy 10.99.0.1:53" tcp 10.99.0.1 53
  expect_reply in_client "tproxy 10.99.0.1:53" udp 10.99.0.1 53
  expect_reply in_client "srv 10.99.0.1:8080" tcp 10.99.0.1 8080
  expect_reply in_client "srv 203.0.113.1:8080" tcp 203.0.113.1 8080 --mark "$own_mark"
  expect_reply in_server "cli 203.0.113.2:9000" tcp 203.0.113.2 9000
  no_leak in_client tcp 2001:db8:1::1 8080
}
intercepted

echo "== kill switch: то же самое, а ICMP заперт"
in_client "$ctl" install --kill-switch
intercepted
if in_client ping -c1 -W2 203.0.113.1 >/dev/null 2>&1; then
  fail "ping прошёл мимо kill switch"
fi

echo "== kill switch, xray остановлен: падаем закрыто"
stop_peers tproxy in_client "$port"
no_leaks in_client "tcp 203.0.113.1 8080" "udp 203.0.113.1 9090" "tcp 10.99.0.1 53"
expect_reply in_client "srv 10.99.0.1:8080" tcp 10.99.0.1 8080
expect_reply in_server "cli 203.0.113.2:9000" tcp 203.0.113.2 9000

echo "== свой список сетей: 203.0.113.0/24 идёт напрямую даже при kill switch"
in_client "$ctl" install --kill-switch --bypass 203.0.113.0/24
expect_reply in_client "srv 203.0.113.1:8080" tcp 203.0.113.1 8080
expect_reply in_client "srv 203.0.113.1:9090" udp 203.0.113.1 9090

echo "== снятие возвращает прямой доступ"
in_client "$ctl" remove --kill-switch --bypass 203.0.113.0/24
assert_clean
expect_reply in_client "srv 203.0.113.1:8080" tcp 203.0.113.1 8080
expect_reply in_client "srv 2001:db8:1::1:8080" tcp 2001:db8:1::1 8080

stream=$root/.github/e2e/stream.py

# alive ГДЕ ФАЙЛ...: соединения целы, tick и pong идут; все файлы проверяются за одну паузу.
alive() {
  local what=$1 out state
  shift
  local states=()
  for state in "$@"; do
    states+=(--state "$state")
  done
  out=$(python3 "$stream" check "${states[@]}" 2>&1) || fail "соединение не пережило: $what ($out)"
  echo "  $what: $out"
}

# watch_stream КТО АДРЕС ПОРТ ФАЙЛ: открывает долгоживущее соединение и ждёт, пока оно встанет.
watch_stream() {
  local who=$1 address=$2 port=$3 state=$4 _
  "$who" python3 "$stream" watch "$address" "$port" --state "$state" >>"$work/peers.log" 2>&1 &
  for _ in $(seq 40); do
    if grep -q '^ok' "$state" 2>/dev/null; then
      return 0
    fi
    sleep 0.25
  done
  fail "не дождались соединения к $address:$port"
}

echo "== соединение к хосту, открытое до правил, живёт при установке, падении xray и снятии"
in_client python3 "$stream" serve --bind 0.0.0.0 --port 9100 >>"$work/peers.log" 2>&1 &
wait_listening in_client t 9100
watch_stream in_server 203.0.113.2 9100 "$work/stream-old.state"
alive "до установки правил" "$work/stream-old.state"
in_client "$ctl" install --kill-switch
alive "правила с kill switch поставлены, соединение было открыто раньше" "$work/stream-old.state"
watch_stream in_server 203.0.113.2 9100 "$work/stream-new.state"
alive "новое входящее соединение при правилах" "$work/stream-new.state"
serve in_client tcp --bind 0.0.0.0 --port "$port" --label tproxy --transparent
serve in_client udp --bind 0.0.0.0 --port "$port" --label tproxy --transparent
wait_listening in_client t "$port"
wait_listening in_client u "$port"
alive "xray запущен, оба соединения" "$work/stream-old.state" "$work/stream-new.state"
stop_peers tproxy in_client "$port"
no_leak in_client tcp 203.0.113.1 8080
alive "xray остановлен, kill switch держит, оба соединения" "$work/stream-old.state" "$work/stream-new.state"
in_client "$ctl" remove --kill-switch
assert_clean
alive "правила сняты, оба соединения" "$work/stream-old.state" "$work/stream-new.state"

lan_flags=(--kill-switch --lan-interface rcl-l --lan-subnets 10.88.0.0/24)
lan_log=$work/lan-server.log
: >"$lan_log"

lan_hits() { wc -l <"$lan_log" | tr -d ' '; }

# lan_no_leak: клиент сети не получает ответа, и сервер ничего не видит.
lan_no_leak() {
  local before out
  before=$(lan_hits)
  if out=$(in_lclient python3 "$peer" probe "$@" 2>/dev/null); then
    fail "неожиданный ответ «$out» на $*"
  fi
  [ "$(lan_hits)" = "$before" ] || fail "запрос устройства дошёл до сервера мимо перехвата: $*"
}

lan_no_leaks() { parallel_no_leak lan_hits in_lclient "$@"; }

lan_rule_count() { in_lrouter ip -4 rule show | grep -c fwmark || true; }

echo "== шлюз для локальной сети: клиент, маршрутизатор и интернет в трёх пространствах"
for ns in "$lan_client" "$lan_router" "$lan_server"; do
  sudo ip netns add "$ns"
  sudo ip netns exec "$ns" ip link set lo up
done
sudo ip link add rcl-c type veth peer name rcl-l
sudo ip link add rcl-w type veth peer name rcl-s
sudo ip link set rcl-c netns "$lan_client"
sudo ip link set rcl-l netns "$lan_router"
sudo ip link set rcl-w netns "$lan_router"
sudo ip link set rcl-s netns "$lan_server"
in_lclient ip link set rcl-c up
in_lrouter ip link set rcl-l up
in_lrouter ip link set rcl-w up
in_lserver ip link set rcl-s up
in_lclient ip addr add 10.88.0.10/24 dev rcl-c
in_lrouter ip addr add 10.88.0.1/24 dev rcl-l
in_lrouter ip addr add 203.0.113.2/24 dev rcl-w
in_lrouter ip addr add 10.99.9.2/24 dev rcl-w
in_lserver ip addr add 203.0.113.1/24 dev rcl-s
in_lserver ip addr add 10.99.9.9/24 dev rcl-s
in_lclient ip route add default via 10.88.0.1
in_lserver ip route add 10.88.0.0/24 via 203.0.113.2
in_lrouter sysctl -qw net.ipv4.conf.all.rp_filter=1
in_lrouter sysctl -qw net.ipv4.ip_forward=1

: >"$work/lan-peers.log"
serve in_lserver tcp --bind 203.0.113.1 --port 8080 --label srv --log "$lan_log"
serve in_lserver udp --bind 203.0.113.1 --port 9090 --label srv --log "$lan_log"
serve in_lserver tcp --bind 10.99.9.9 --port 8080 --label srv --log "$lan_log"
serve in_lrouter tcp --bind 10.88.0.1 --port 8080 --label rtr
in_lrouter python3 "$stream" serve --bind 0.0.0.0 --port 9100 >>"$work/peers.log" 2>&1 &
wait_listening in_lserver t 8080
wait_listening in_lserver u 9090
wait_listening in_lrouter t 8080
wait_listening in_lrouter t 9100

echo "== стенд: без правил маршрутизатор с ip_forward пересылает всё"
expect_reply in_lclient "srv 203.0.113.1:8080" tcp 203.0.113.1 8080
expect_reply in_lclient "srv 10.99.9.9:8080" tcp 10.99.9.9 8080
in_lclient ping -c1 -W2 203.0.113.1 >/dev/null || fail "стенд не пересылает ICMP"
watch_stream in_lserver 203.0.113.2 9100 "$work/lan-stream-wan.state"
watch_stream in_lclient 10.88.0.1 9100 "$work/lan-stream-lan.state"
lan_streams() {
  alive "$1 (из интернета и из сети к хосту)" "$work/lan-stream-wan.state" "$work/lan-stream-lan.state"
}
lan_streams "до правил"

echo "== ip_forward=0: перехват работает без пересылки, демон sysctl не трогает"
in_lrouter sysctl -qw net.ipv4.ip_forward=0
in_lrouter "$ctl" install "${lan_flags[@]}"
[ "$(in_lrouter cat /proc/sys/net/ipv4/ip_forward)" = 0 ] || fail "установка правил изменила ip_forward"
[ "$(lan_rule_count)" = 1 ] || fail "после install не одно правило ip rule"
serve in_lrouter tcp --bind 0.0.0.0 --port "$port" --label lantproxy --transparent
serve in_lrouter udp --bind 0.0.0.0 --port "$port" --label lantproxy --transparent
wait_listening in_lrouter t "$port"
wait_listening in_lrouter u "$port"
lan_intercepted() {
  expect_reply in_lclient "lantproxy 203.0.113.1:8080" tcp 203.0.113.1 8080
  expect_reply in_lclient "lantproxy 203.0.113.1:9090" udp 203.0.113.1 9090
  expect_reply in_lclient "lantproxy 10.88.0.1:53" udp 10.88.0.1 53
  expect_reply in_lclient "lantproxy 10.88.0.1:53" tcp 10.88.0.1 53
  expect_reply in_lclient "lantproxy 10.99.9.9:53" udp 10.99.9.9 53
  expect_reply in_lclient "rtr 10.88.0.1:8080" tcp 10.88.0.1 8080
}
lan_intercepted
lan_no_leak tcp 10.99.9.9 8080
lan_streams "правила стоят, xray запущен"

echo "== ip_forward=1: наружу пересылается только то, что не публичное"
in_lrouter sysctl -qw net.ipv4.ip_forward=1
lan_intercepted
expect_reply in_lclient "srv 10.99.9.9:8080" tcp 10.99.9.9 8080
if in_lclient ping -c1 -W2 203.0.113.1 >/dev/null 2>&1; then
  fail "ICMP устройства вышел наружу мимо kill switch"
fi
lan_no_leak tcp 10.88.0.1 12345
lan_streams "пересылка включена"

echo "== xray остановлен: устройства закрыты, хост доступен"
stop_peers lantproxy in_lrouter "$port"
lan_no_leaks "tcp 203.0.113.1 8080" "udp 203.0.113.1 9090" "udp 10.88.0.1 53" "tcp 10.88.0.1 53"
expect_reply in_lclient "rtr 10.88.0.1:8080" tcp 10.88.0.1 8080
lan_streams "xray остановлен"

echo "== правило маршрутизации пропало: пересылка всё равно закрыта"
in_lrouter ip -4 rule del fwmark "$(default intercept_mark)" lookup "$table" priority "$(default priority)"
[ "$(lan_rule_count)" = 0 ] || fail "правило маршрутизации не удалилось"
lan_no_leaks "tcp 203.0.113.1 8080" "udp 203.0.113.1 9090"
lan_streams "правило маршрутизации пропало"
in_lrouter "$ctl" install "${lan_flags[@]}"
[ "$(lan_rule_count)" = 1 ] || fail "повторная установка не вернула правило"
lan_streams "правила переустановлены"

echo "== снятие правил: хост как был, устройства снова ходят напрямую"
in_lrouter "$ctl" remove "${lan_flags[@]}"
[ -z "$(in_lrouter nft list ruleset)" ] || fail "после remove остались правила nft"
[ "$(lan_rule_count)" = 0 ] || fail "после remove осталось правило ip rule"
expect_reply in_lclient "srv 203.0.113.1:8080" tcp 203.0.113.1 8080
lan_streams "правила сняты"

echo "готово"
