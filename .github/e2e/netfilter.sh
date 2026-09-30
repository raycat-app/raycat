#!/usr/bin/env bash
# e2e правил перехвата на настоящем ядре: два сетевых пространства, соединённых veth.
# В «клиенте» стоят правила, «сервер» изображает интернет и приватную сеть. Роль xray
# играет прозрачный сервер netfilter_peer.py.
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
work=$(mktemp -d)

fail() {
  echo "ПРОВАЛ: $*" >&2
  exit 1
}

in_client() { sudo ip netns exec "$client" "$@"; }
in_server() { sudo ip netns exec "$server" "$@"; }

cleanup() {
  local status=$?
  set +e
  if [ "$status" -ne 0 ]; then
    for file in server.log peers.log; do
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
  fi
  for ns in "$client" "$server"; do
    sudo ip netns pids "$ns" 2>/dev/null | xargs -r sudo kill 2>/dev/null
  done
  sudo ip netns del "$client" 2>/dev/null
  sudo ip netns del "$server" 2>/dev/null
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
no_leak in_client tcp 203.0.113.1 8080
no_leak in_client udp 203.0.113.1 9090
no_leak in_client tcp 2001:db8:1::1 8080
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
sudo pkill -f '[l]abel tproxy' || true
sleep 1
no_leak in_client tcp 203.0.113.1 8080
no_leak in_client udp 203.0.113.1 9090
no_leak in_client tcp 10.99.0.1 53
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

echo "готово"
