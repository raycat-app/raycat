#!/usr/bin/env bash
# e2e режима шлюза: приложение в сетевом пространстве шлюза ходит на сайт только
# через узел; kill switch, DNS, IPv6, восстановление правил и их снятие.
#
#   docker load < образ   # тег raycat:ci
#   bash .github/e2e/gateway.sh
#
# Стенд описан в .github/e2e/gateway/compose.yml. Нужен Docker с compose v2.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
compose=(docker compose -f "$here/gateway/compose.yml")
image=${RAYCAT_IMAGE:-raycat:ci}

gateway=raycat-e2e-gateway
app=raycat-e2e-app
holder=raycat-e2e-holder
gw2=raycat-e2e-gw2
site=http://site.e2e.test/
canary=http://11.30.0.50/
nodes_inet=(11.40.0.11 11.40.0.12)
nodes_wan=(11.30.0.11 11.30.0.12)

fail() {
  echo "ПРОВАЛ: $*" >&2
  exit 1
}

cleanup() {
  local status=$?
  set +e
  if [ "$status" -ne 0 ]; then
    docker ps -a --format 'table {{.Names}}\t{{.Status}}' >&2
    for name in "$gateway" "$gw2"; do
      echo "::group::журнал $name"
      docker logs "$name" 2>&1 | tail -80
      echo "::endgroup::"
    done
    echo "::group::таблица и правила шлюза"
    docker exec "$gateway" nft list ruleset 2>&1
    docker exec "$gateway" ip rule 2>&1
    echo "::endgroup::"
  fi
  "${compose[@]}" --profile lifecycle down --volumes --remove-orphans --timeout 3 >/dev/null 2>&1
  docker rm -f raycat-e2e-leak >/dev/null 2>&1
  exit "$status"
}
trap cleanup EXIT

app_run() { docker exec "$app" "$@"; }
gateway_run() { docker exec "$gateway" "$@"; }
fetch() { app_run wget -q -T 5 -O - "$1"; }

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
rule_present() { grep -q 7263 <("$@" ip -4 rule show); }

# Ответ сайта — адрес, с которого к нему пришли; он должен принадлежать узлу.
expect_via_node() {
  local url=$1
  shift
  local seen
  seen=$(fetch "$url") || fail "$url не отвечает через шлюз"
  in_list "$seen" "$@" || fail "$url ответил адресом $seen, ожидался адрес узла: $*"
  echo "  $url: пришли с адреса узла $seen"
}

echo "== образ и стенд"
docker image inspect "$image" >/dev/null || fail "нет образа $image"
[ "$image" = raycat:ci ] || docker tag "$image" raycat:ci
"${compose[@]}" up --detach --wait --wait-timeout 180 >/dev/null
docker ps --format 'table {{.Names}}\t{{.Status}}'

echo "== приложение ходит через узел"
wait_for "сайт через шлюз" 60 fetch "$site"
expect_via_node "$site" "${nodes_inet[@]}"
expect_via_node "$canary" "${nodes_wan[@]}"
log_has "$gateway" "правила перехвата установлены, kill switch включён" || fail "в журнале нет строки об установке правил"
[ "$(docker logs "$gateway" 2>&1 | grep -c 'xray запущен')" -eq 1 ] || fail "xray запускался не один раз"
gateway_log=$(docker logs "$gateway" 2>&1)
first_event=$(grep -m 1 -e 'правила перехвата установлены' -e 'xray запущен' <<<"$gateway_log")
case "$first_event" in
  *"правила перехвата установлены"*) ;;
  *) fail "xray запущен раньше, чем установлены правила" ;;
esac

echo "== DNS приложения отвечает fake-IP (пул 198.18.0.0/15)"
answer=$(app_run nslookup site.e2e.test) || fail "nslookup не ответил"
echo "$answer" | grep -q 'Address: 198\.1[89]\.' || fail "DNS ответил не fake-IP: $answer"
outside=$(app_run nslookup site.e2e.test 1.1.1.1) || fail "nslookup к 1.1.1.1 не ответил"
echo "$outside" | grep -q 'Address: 198\.1[89]\.' || fail "DNS к внешнему серверу ответил не fake-IP: $outside"

echo "== IPv6 наружу не выходит"
table=$(gateway_run nft list table inet raycat)
grep -q 'nfproto ipv6' <<<"$table" || fail "в таблице нет запрета IPv6"
if app_run wget -q -T 4 -O - 'http://[2606:4700:4700::1111]/' >/dev/null 2>&1; then
  fail "IPv6 вышел наружу"
fi
if app_run ping -6 -c 1 -W 2 2606:4700:4700::1111 >/dev/null 2>&1; then
  fail "IPv6 ping вышел наружу"
fi

echo "== правила возвращаются, если их сбросили"
gateway_run ip -4 rule del fwmark 0x52540000 lookup 7263 priority 7263
if rule_present docker exec "$gateway"; then
  fail "правило маршрутизации не удалилось"
fi
wait_for "правило маршрутизации вернулось" 50 rule_present docker exec "$gateway"
log_has "$gateway" "правила перехвата пропали" || fail "в журнале нет предупреждения о пропаже правил"
log_has "$gateway" "правила перехвата восстановлены" || fail "в журнале нет строки о восстановлении"
expect_via_node "$site" "${nodes_inet[@]}"

echo "== kill switch: xray убит, демон заморожен"
docker kill --signal STOP "$gateway" >/dev/null
gateway_run sh -c 'kill -9 $(pidof xray)'
sleep 1
if leaked=$(app_run wget -q -T 4 -O - "$canary" 2>&1); then
  fail "трафик приложения ушёл мимо xray: $leaked"
fi
if leaked=$(app_run wget -q -T 4 -O - "$site" 2>&1); then
  fail "трафик по имени прошёл без xray: $leaked"
fi
if app_run timeout 8 nslookup site.e2e.test 1.1.1.1 >/dev/null 2>&1; then
  fail "DNS-запрос вышел наружу без xray"
fi
if app_run ping -c 1 -W 2 11.30.0.50 >/dev/null 2>&1; then
  fail "ICMP вышел наружу без xray"
fi
echo "  без xray трафик, DNS и ICMP приложения отклоняются"
docker kill --signal CONT "$gateway" >/dev/null
wait_for "xray перезапущен и трафик снова идёт" 60 fetch "$site"
expect_via_node "$site" "${nodes_inet[@]}"
log_has "$gateway" "xray завершился" || fail "в журнале нет записи о падении xray"

echo "== docker compose restart: приложение снова ходит через шлюз"
"${compose[@]}" restart raycat >/dev/null
"${compose[@]}" up --detach --wait --wait-timeout 120 >/dev/null
wait_for "сайт после перезапуска шлюза" 60 fetch "$site"
expect_via_node "$site" "${nodes_inet[@]}"

echo "== без явного dns: шлюз отказывается стартовать"
set +e
leak_output=$(docker run --rm --name raycat-e2e-leak --network raycat-e2e-wan --ip 11.30.0.4 \
  --cap-add NET_ADMIN --volume "$here:/e2e:ro" --env RAYCAT_CONFIG=/e2e/gateway/config.toml \
  "$image" daemon 2>&1)
leak_code=$?
set -e
echo "$leak_output"
[ "$leak_code" -ne 0 ] || fail "шлюз с утечкой DNS стартовал"
echo "$leak_output" | grep -q "Docker разрешает имена" || fail "нет сообщения об утечке DNS"
echo "$leak_output" | grep -q "правила перехвата установлены" && fail "правила поставлены несмотря на утечку"

echo "== без привилегии NET_ADMIN шлюз отказывается стартовать"
set +e
cap_output=$(docker run --rm --name raycat-e2e-leak --network raycat-e2e-wan --ip 11.30.0.4 \
  --dns 1.1.1.1 --volume "$here:/e2e:ro" --env RAYCAT_CONFIG=/e2e/gateway/config.toml \
  "$image" daemon 2>&1)
cap_code=$?
set -e
echo "$cap_output"
[ "$cap_code" -ne 0 ] || fail "шлюз без CAP_NET_ADMIN стартовал"
echo "$cap_output" | grep -q "CAP_NET_ADMIN" || fail "нет сообщения о CAP_NET_ADMIN"

echo "== штатная остановка снимает правила, авария оставляет"
"${compose[@]}" --profile lifecycle up --detach holder gw2 >/dev/null
wait_for "правила в пространстве holder" 60 docker exec "$holder" nft list table inet raycat
wait_for "xray в gw2" 60 docker exec "$gw2" pidof xray
docker stop "$gw2" >/dev/null
code=$(docker inspect -f '{{.State.ExitCode}}' "$gw2")
[ "$code" = 0 ] || fail "gw2 завершился с кодом $code после docker stop"
log_has "$gw2" "правила перехвата сняты" || fail "в журнале нет строки о снятии правил"
if docker exec "$holder" nft list table inet raycat >/dev/null 2>&1; then
  fail "таблица nftables осталась после штатной остановки"
fi
if rule_present docker exec "$holder"; then
  fail "правило маршрутизации осталось после штатной остановки"
fi
echo "  после docker stop правил нет"
docker start "$gw2" >/dev/null
wait_for "правила после повторного запуска" 60 docker exec "$holder" nft list table inet raycat
docker kill --signal KILL "$gw2" >/dev/null
sleep 1
docker exec "$holder" nft list table inet raycat >/dev/null || fail "после аварийного выхода правила исчезли"
rule_present docker exec "$holder" || fail "после аварийного выхода исчезло правило маршрутизации"
echo "  после аварийного выхода правила остались (kill switch)"

echo "e2e шлюза: успех"
