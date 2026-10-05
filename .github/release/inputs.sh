#!/usr/bin/env bash
# Готовит файлы для package.sh из артефактов CI. Запускать из корня репозитория.
#   inputs.sh <каталог артефактов> <каталог результата>
# Ожидает в каталоге артефактов: bin/raycat-<цель>/raycat, image/image-arm64-noaes.tar.gz,
# xray-noaes-armv7/xray. Обычный xray берётся из архивов релиза по контрольным суммам
# versions.toml (те же, что у образа), xray без AES для aarch64 — из образа CI.
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

artifacts=${1:?не указан каталог артефактов}
out=${2:?не указан каталог результата}
mkdir -p "$out"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for pair in x86_64:x86_64-unknown-linux-musl aarch64:aarch64-unknown-linux-musl armv7:armv7-unknown-linux-musleabihf; do
  key=${pair%%:*}
  target=${pair#*:}
  install -m 755 "$artifacts/bin/raycat-$target/raycat" "$out/raycat-$key"
done
install -m 755 "$artifacts/xray-noaes-armv7/xray" "$out/xray-armv7-noaes"

xray_version=$(sed -n 's/^version = "\(.*\)"/\1/p' versions.toml)
[ -n "$xray_version" ] || fail "в versions.toml нет версии xray"
for pair in x86_64:Xray-linux-64.zip aarch64:Xray-linux-arm64-v8a.zip armv7:Xray-linux-arm32-v7a.zip; do
  key=${pair%%:*}
  archive=${pair#*:}
  sha256=$(sed -n "s/^$key = \"\(.*\)\"/\1/p" versions.toml)
  [ -n "$sha256" ] || fail "в versions.toml нет суммы для $key"
  curl -fsSL -o "$tmp/$archive" "https://github.com/XTLS/Xray-core/releases/download/v${xray_version}/${archive}"
  echo "$sha256  $tmp/$archive" | sha256sum -c -
  unzip -q -o "$tmp/$archive" xray -d "$tmp/$key"
  install -m 755 "$tmp/$key/xray" "$out/xray-$key"
done

gunzip -c "$artifacts/image/image-arm64-noaes.tar.gz" | docker load
container=$(docker create --platform linux/arm64 raycat:ci)
docker cp "$container:/usr/libexec/raycat/xray" "$tmp/xray-aarch64-noaes"
docker rm "$container" >/dev/null
docker rmi raycat:ci >/dev/null
install -m 755 "$tmp/xray-aarch64-noaes" "$out/xray-aarch64-noaes"

raycat="$out/raycat-x86_64"
"$raycat" completions bash >"$out/raycat.bash"
"$raycat" completions zsh >"$out/_raycat"
"$raycat" completions fish >"$out/raycat.fish"
"$raycat" man >"$out/raycat.1"
for generated in raycat.bash _raycat raycat.fish raycat.1; do
  [ -s "$out/$generated" ] || fail "$generated пустой"
done

[ -f deploy/raycat.service ] || fail "нет deploy/raycat.service"
install -m 644 deploy/raycat.service "$out/raycat.service"
install -m 644 LICENSE README.md "$out/"
