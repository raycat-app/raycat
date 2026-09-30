#!/usr/bin/env bash
# Бенчмарк версий xray на одном раннере. Клиент (socks), сервер (vless+reality и
# shadowsocks 2022), сайт и TLS-заглушка для reality работают на этой же машине.
#
#   VERSIONS="26.3.27 26.9.9" SIZE_MIB=500 OUT_DIR=bench-out bash .github/bench/bench.sh
#   bash .github/bench/bench.sh summary bench-out [другой-каталог ...]
#
# Версии замеряются вперемешку: сначала по одному скачиванию у каждой, потом
# следующий круг (порядок в чётных кругах обратный), чтобы шум раннера не достался
# одной версии. Нужен sudo для `ip addr add`: как и в e2e-стенде, узел и сайт
# слушают адреса на lo, потому что freedom в xray не ходит на зарезервированные
# диапазоны.
set -euo pipefail
export LC_ALL=C

header='| Архитектура | Версия xray | Протокол | Скорость, Мбит/с | Разброс, Мбит/с | К первой версии | CPU клиента, % | CPU сервера, % | CPU, с/ГиБ | RSS клиента, МиБ | RSS сервера, МиБ |'
separator='|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|'

fail() {
  echo "ПРОВАЛ: $*" >&2
  exit 1
}

if [ "${1:-run}" = summary ]; then
  shift
  [ "$#" -gt 0 ] || fail "не указаны каталоги с результатами"
  for dir in "$@"; do
    cat "$dir/info.md"
  done
  echo
  echo "$header"
  echo "$separator"
  for dir in "$@"; do
    cat "$dir/rows.md"
  done
  exit 0
fi

versions=${VERSIONS:?не заданы версии xray}
size=${SIZE_MIB:-500}
out=${OUT_DIR:-bench-out}
repeats=3

read -ra vers <<<"$versions"
[ "${#vers[@]}" -gt 0 ] || fail "пустой список версий"
for v in "${vers[@]}"; do
  [[ $v =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "неверная версия: $v"
done
[[ $size =~ ^[0-9]+$ ]] && [ "$size" -ge 1 ] && [ "$size" -le 4096 ] || fail "неверный объём: $size"

case "$(uname -m)" in
  x86_64)
    arch=x86_64
    archive=Xray-linux-64.zip
    ;;
  aarch64)
    arch=aarch64
    archive=Xray-linux-arm64-v8a.zip
    ;;
  *) fail "неподдерживаемая архитектура: $(uname -m)" ;;
esac

mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
clk=$(getconf CLK_TCK)

node_ip=11.11.11.10
site_ip=11.11.11.20
site_port=18080
dest_port=18443
url="http://$site_ip:$site_port/data.bin"
expected=$((size * 1048576))

pids=()
srv_pid=()
cli_pid=()

cleanup() {
  local status=$?
  set +e
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null; done
  wait 2>/dev/null
  if [ "$status" -ne 0 ]; then
    for log in "$work"/*.log; do
      echo "::group::$(basename "$log")"
      tail -n 50 "$log"
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

fetch() {
  local version=$1 dir="$work/xray-$1" base want
  base="https://github.com/XTLS/Xray-core/releases/download/v$version"
  mkdir -p "$dir"
  curl -fsSL --retry 3 -o "$dir/$archive" "$base/$archive"
  curl -fsSL --retry 3 -o "$dir/$archive.dgst" "$base/$archive.dgst"
  want=$(awk -F= 'toupper($1) ~ /^SHA(2-)?256/ {gsub(/[[:space:]]/, "", $2); print tolower($2); exit}' "$dir/$archive.dgst")
  [ "${#want}" -eq 64 ] || fail "в $archive.dgst для $version нет sha256"
  echo "$want  $dir/$archive" | sha256sum -c - >/dev/null || fail "sha256 $archive $version не совпал с .dgst"
  unzip -q "$dir/$archive" xray -d "$dir"
  "$dir/xray" version >"$dir/version.txt"
  head -n 1 "$dir/version.txt"
}

ticks() { awk '{print $14 + $15}' "/proc/$1/stat"; }
rss_mib() { awk '/^VmHWM:/ {printf "%.1f", $2 / 1024}' "/proc/$1/status"; }

mean_speed() {
  awk '{s += $1 * 8 / 1e6; n++} END {printf "%.3f", s / n}' "$1"
}

for v in "${vers[@]}"; do
  echo "== xray $v"
  fetch "$v"
done

sudo ip addr add "$node_ip/32" dev lo
sudo ip addr add "$site_ip/32" dev lo

echo "== сайт: ${size} МиБ"
mkdir "$work/site"
head -c "$expected" /dev/urandom >"$work/site/data.bin"
echo ok >"$work/site/small.txt"
cat "$work/site/data.bin" >/dev/null
python3 -m http.server "$site_port" --bind "$site_ip" --directory "$work/site" >"$work/site.log" 2>&1 &
pids+=($!)

openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 1 \
  -subj "/CN=www.example.com" -keyout "$work/dest.key" -out "$work/dest.crt" 2>/dev/null
python3 "$(dirname "$0")/tls_dest.py" --port "$dest_port" --cert "$work/dest.crt" --key "$work/dest.key" \
  >"$work/dest.log" 2>&1 &
pids+=($!)

wait_for "сайт" curl -fsS "http://$site_ip:$site_port/small.txt"
wait_for "TLS-заглушка" bash -c "exec 3<>/dev/tcp/127.0.0.1/$dest_port"

# Формат вывода x25519 отличается между версиями; в обоих сначала закрытый ключ,
# затем открытый.
keys=$("$work/xray-${vers[0]}/xray" x25519)
private_key=$(printf '%s\n' "$keys" | awk -F': *' 'NR == 1 {gsub(/[[:space:]]/, "", $2); print $2}')
public_key=$(printf '%s\n' "$keys" | awk -F': *' 'NR == 2 {gsub(/[[:space:]]/, "", $2); print $2}')
[ -n "$private_key" ] && [ -n "$public_key" ] || fail "не удалось разобрать вывод xray x25519"
uuid=$(cat /proc/sys/kernel/random/uuid)
short_id=$(openssl rand -hex 8)
ss_key=$(openssl rand -base64 16)

write_configs() {
  local proto=$1 i=$2 server_port=$3 client_port=$4 dir=$5 inbound outbound
  case "$proto" in
    vless-reality)
      inbound='{"listen":"'"$node_ip"'","port":'"$server_port"',"protocol":"vless",
        "settings":{"clients":[{"id":"'"$uuid"'","flow":"xtls-rprx-vision"}],"decryption":"none"},
        "streamSettings":{"network":"tcp","security":"reality","realitySettings":{
          "dest":"127.0.0.1:'"$dest_port"'","serverNames":["www.example.com"],
          "privateKey":"'"$private_key"'","shortIds":["'"$short_id"'"]}}}'
      outbound='{"protocol":"vless",
        "settings":{"vnext":[{"address":"'"$node_ip"'","port":'"$server_port"',
          "users":[{"id":"'"$uuid"'","encryption":"none","flow":"xtls-rprx-vision"}]}]},
        "streamSettings":{"network":"tcp","security":"reality","realitySettings":{
          "serverName":"www.example.com","fingerprint":"chrome","publicKey":"'"$public_key"'",
          "shortId":"'"$short_id"'","spiderX":"/"}}}'
      ;;
    shadowsocks-2022)
      inbound='{"listen":"'"$node_ip"'","port":'"$server_port"',"protocol":"shadowsocks",
        "settings":{"method":"2022-blake3-aes-128-gcm","password":"'"$ss_key"'","network":"tcp"}}'
      outbound='{"protocol":"shadowsocks",
        "settings":{"servers":[{"address":"'"$node_ip"'","port":'"$server_port"',
          "method":"2022-blake3-aes-128-gcm","password":"'"$ss_key"'"}]}}'
      ;;
  esac
  cat >"$dir/server-$i.json" <<EOF
{"log":{"loglevel":"warning"},"inbounds":[$inbound],"outbounds":[{"protocol":"freedom"}]}
EOF
  cat >"$dir/client-$i.json" <<EOF
{"log":{"loglevel":"warning"},
 "inbounds":[{"listen":"127.0.0.1","port":$client_port,"protocol":"socks","settings":{"auth":"noauth","udp":false}}],
 "outbounds":[$outbound]}
EOF
}

start_pair() {
  local proto=$1 i=$2 base=$3 bin="$work/xray-${vers[$2]}/xray"
  local server_port=$((base + i)) client_port=$((base + 50 + i))
  write_configs "$proto" "$i" "$server_port" "$client_port" "$work"
  "$bin" run -test -c "$work/server-$i.json" >"$work/test-server-$i.log" 2>&1
  "$bin" run -test -c "$work/client-$i.json" >"$work/test-client-$i.log" 2>&1
  "$bin" run -c "$work/server-$i.json" >"$work/server-$proto-$i.log" 2>&1 &
  srv_pid[i]=$!
  "$bin" run -c "$work/client-$i.json" >"$work/client-$proto-$i.log" 2>&1 &
  cli_pid[i]=$!
  pids+=("${srv_pid[i]}" "${cli_pid[i]}")
  wait_for "прокси $proto ${vers[$i]}" \
    curl -fsS --max-time 5 --socks5-hostname "127.0.0.1:$client_port" "http://$site_ip:$site_port/small.txt"
}

measure() {
  local proto=$1 i=$2 base=$3 line c0 s0 c1 s1 got
  c0=$(ticks "${cli_pid[i]}")
  s0=$(ticks "${srv_pid[i]}")
  line=$(curl -fsS --max-time 900 --socks5-hostname "127.0.0.1:$((base + 50 + i))" -o /dev/null \
    -w '%{speed_download} %{time_total} %{size_download}' "$url") ||
    fail "скачивание через $proto ${vers[$i]} не удалось"
  c1=$(ticks "${cli_pid[i]}")
  s1=$(ticks "${srv_pid[i]}")
  got=${line##* }
  [ "$got" -eq "$expected" ] || fail "через $proto ${vers[$i]} скачано $got байт из $expected"
  echo "$line $((c1 - c0)) $((s1 - s0))" >>"$work/runs-$proto-$i.txt"
}

print_row() {
  local proto=$1 i=$2 first=$3 file="$work/runs-$1-$2.txt"
  awk -v arch="$arch" -v ver="${vers[$i]}" -v proto="$proto" -v clk="$clk" -v first="$first" \
    -v rss_c="$(rss_mib "${cli_pid[i]}")" -v rss_s="$(rss_mib "${srv_pid[i]}")" '
    {
      sp = $1 * 8 / 1e6; sum += sp; n++
      if (n == 1 || sp < min) min = sp
      if (n == 1 || sp > max) max = sp
      t += $2; bytes += $3; dc += $4; ds += $5
    }
    END {
      mean = sum / n
      delta = (first > 0) ? sprintf("%+.1f %%", (mean / first - 1) * 100) : "—"
      printf "| %s | %s | %s | %.1f | %.1f–%.1f | %s | %.0f | %.0f | %.1f | %s | %s |\n",
        arch, ver, proto, mean, min, max, delta,
        100 * dc / clk / t, 100 * ds / clk / t, (dc + ds) / clk / (bytes / 1073741824), rss_c, rss_s
    }' "$file"
}

: >"$out/rows.md"

echo "== без прокси"
for _ in $(seq "$repeats"); do
  curl -fsS --max-time 900 -o /dev/null -w '%{speed_download} %{time_total} %{size_download}\n' "$url" \
    >>"$work/direct.txt"
done
awk -v arch="$arch" '
  {s = $1 * 8 / 1e6; sum += s; n++; if (n == 1 || s < min) min = s; if (n == 1 || s > max) max = s}
  END {printf "| %s | — | без прокси | %.1f | %.1f–%.1f | — | — | — | — | — | — |\n", arch, sum / n, min, max}
' "$work/direct.txt" >>"$out/rows.md"

pi=0
for proto in vless-reality shadowsocks-2022; do
  echo "== $proto"
  base=$((21000 + pi * 100))
  for i in "${!vers[@]}"; do
    start_pair "$proto" "$i" "$base"
  done
  for rep in $(seq "$repeats"); do
    if [ $((rep % 2)) -eq 0 ]; then
      mapfile -t order < <(printf '%s\n' "${!vers[@]}" | tac)
    else
      order=("${!vers[@]}")
    fi
    for i in "${order[@]}"; do
      measure "$proto" "$i" "$base"
    done
  done
  first=0
  for i in "${!vers[@]}"; do
    print_row "$proto" "$i" "$first" >>"$out/rows.md"
    [ "$i" -eq 0 ] && first=$(mean_speed "$work/runs-$proto-0.txt")
    kill "${srv_pid[i]}" "${cli_pid[i]}" 2>/dev/null || true
  done
  pi=$((pi + 1))
done

model=$(lscpu | awk -F': *' '/^Model name/ {print $2; exit}')
{
  printf '%s\n' "- ${arch}: ${model:-процессор неизвестен}, ядер $(nproc), ядро $(uname -r), ${size} МиБ на скачивание, ${repeats} повтора"
  [ -z "${GODEBUG:-}" ] || printf '%s\n' "- GODEBUG=${GODEBUG} (у xray, сайта и curl)"
} >"$out/info.md"

bash "$0" summary "$out"
