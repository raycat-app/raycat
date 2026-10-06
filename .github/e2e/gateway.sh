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
  docker rm -f raycat-e2e-leak raycat-e2e-unready >/dev/null 2>&1
  rm -rf "$scratch"
  exit "$status"
}
trap cleanup EXIT

scratch=$(mktemp -d)

app_run() { docker exec "$app" "$@"; }
gateway_run() { docker exec "$gateway" "$@"; }
fetch() { app_run wget -q -T 5 -O - "$1"; }

# Опрос раз в 0,2 с; второй аргумент — предел в секундах.
wait_for() {
  local what=$1 limit=$2
  shift 2
  local deadline=$((SECONDS + limit))
  while :; do
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    [ "$SECONDS" -lt "$deadline" ] || break
    sleep 0.2
  done
  fail "не дождались: $what"
}

# Проверки «наружу ничего не уходит» ждут тайм-аутов, поэтому идут одновременно:
# refuse запускает команду в фоне, refused_all требует, чтобы каждая завершилась ошибкой.
refuse_jobs=()
refuse_names=()
refuse() {
  local what=$1
  shift
  "$@" >"$scratch/refuse.${#refuse_jobs[@]}" 2>&1 &
  refuse_jobs+=($!)
  refuse_names+=("$what")
}
refused_all() {
  local i
  for i in "${!refuse_jobs[@]}"; do
    if wait "${refuse_jobs[$i]}"; then
      fail "${refuse_names[$i]}: $(cat "$scratch/refuse.$i")"
    fi
  done
  refuse_jobs=()
  refuse_names=()
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

echo "== жёсткие настройки и готовность: read_only, HEALTHCHECK, raycat health, CLI внутри контейнера"
[ "$(docker inspect -f '{{.HostConfig.ReadonlyRootfs}}' "$gateway")" = true ] || fail "корень шлюза не только для чтения"
if gateway_run sh -c 'touch /probe' 2>/dev/null; then
  fail "в корень шлюза удалось записать файл"
fi
gateway_run test -S /var/lib/raycat/raycat.sock || fail "сокет API не в каталоге состояния"
health_line=$(gateway_run raycat health 2>&1) || fail "raycat health в работающем шлюзе: $health_line"
grep -q '^готов: xray работает' <<<"$health_line" || fail "неожиданный вывод raycat health: $health_line"
echo "  $health_line"
[ "$(docker inspect -f '{{.State.Health.Status}}' "$gateway")" = healthy ] || fail "HEALTHCHECK образа не считает шлюз здоровым"
status_json=$(gateway_run raycat status --json) || fail "raycat status внутри контейнера не нашёл демон: $status_json"
grep -q '"mode": "gateway"' <<<"$status_json" || fail "raycat status ответил не режимом шлюза: $status_json"

echo "== DNS приложения отвечает fake-IP (пул 198.18.0.0/15)"
answer=$(app_run nslookup site.e2e.test) || fail "nslookup не ответил"
echo "$answer" | grep -q 'Address: 198\.1[89]\.' || fail "DNS ответил не fake-IP: $answer"
outside=$(app_run nslookup site.e2e.test 1.1.1.1) || fail "nslookup к 1.1.1.1 не ответил"
echo "$outside" | grep -q 'Address: 198\.1[89]\.' || fail "DNS к внешнему серверу ответил не fake-IP: $outside"

echo "== IPv6 наружу не выходит"
table=$(gateway_run nft list table inet raycat)
# IPv6 не перехватывается и с kill switch попадает под завершающий reject цепочки guard.
grep -Eq '^[[:space:]]+reject$' <<<"$table" || fail "в таблице нет завершающего reject"
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
refuse "трафик приложения ушёл мимо xray" app_run wget -q -T 4 -O - "$canary"
refuse "трафик по имени прошёл без xray" app_run wget -q -T 4 -O - "$site"
refuse "DNS-запрос вышел наружу без xray" app_run timeout 8 nslookup site.e2e.test 1.1.1.1
refuse "ICMP вышел наружу без xray" app_run ping -c 1 -W 2 11.30.0.50
refused_all
echo "  без xray трафик, DNS и ICMP приложения отклоняются"
started=$SECONDS
if health_out=$(gateway_run raycat health 2>&1); then
  fail "raycat health ответил успехом, хотя демон заморожен, а xray убит: $health_out"
fi
[ $((SECONDS - started)) -le 5 ] || fail "raycat health отвечал дольше 5 с"
grep -q 'демон не ответил' <<<"$health_out" || fail "неожиданная причина в raycat health: $health_out"
echo "  raycat health: $health_out"
docker kill --signal CONT "$gateway" >/dev/null
wait_for "xray перезапущен и трафик снова идёт" 60 fetch "$site"
wait_for "raycat health снова успешен" 60 gateway_run raycat health
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

echo "== демон отвечает, но подписка недоступна: raycat health отказывает с причиной"
docker run --detach --name raycat-e2e-unready --network raycat-e2e-wan --read-only --cap-drop ALL \
  --security-opt no-new-privileges:true --tmpfs /tmp --tmpfs /var/lib/raycat \
  --volume "$here:/e2e:ro" --env RAYCAT_CONFIG=/e2e/gateway/unready.toml "$image" >/dev/null
unready_out=
for _ in $(seq 60); do
  if unready_out=$(docker exec raycat-e2e-unready raycat health 2>&1); then
    fail "raycat health успешен без единого узла: $unready_out"
  fi
  if grep -Eq 'xray не запущен|узел не выбран' <<<"$unready_out"; then
    break
  fi
  sleep 1
done
grep -Eq 'xray не запущен|узел не выбран' <<<"$unready_out" || fail "raycat health не назвал причину неготовности: $unready_out"
echo "  raycat health: $unready_out"
docker rm -f raycat-e2e-unready >/dev/null

echo "== файл настроек 0600 чужого владельца и cap_drop ALL: понятная подсказка"
perm_dir=$(mktemp -d)
cp "$here/gateway/config.toml" "$perm_dir/config.toml"
chmod 755 "$perm_dir"
chmod 600 "$perm_dir/config.toml"
perm_check() {
  docker run --rm --read-only --cap-drop ALL --security-opt no-new-privileges:true --tmpfs /tmp \
    --tmpfs /var/lib/raycat --volume "$perm_dir:/conf:ro" --env RAYCAT_CONFIG=/conf/config.toml \
    "$image" check 2>&1 || true
}
perm_output=$(perm_check)
echo "$perm_output"
grep -q 'chmod 644' <<<"$perm_output" || fail "нет подсказки про права файла настроек"
chmod 644 "$perm_dir/config.toml"
perm_output=$(perm_check)
if grep -q 'Permission denied' <<<"$perm_output"; then
  fail "файл с правами 644 не читается: $perm_output"
fi
rm -rf "$perm_dir"

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
