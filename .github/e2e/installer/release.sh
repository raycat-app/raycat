#!/usr/bin/env bash
# Собирает ресурсы выпуска в формате контракта (архивы всех целей и SHA256SUMS) для
# проверки установщика без GitHub.
#
#   bash .github/e2e/installer/release.sh <каталог> <версия> <raycat> <xray> [pad]
#
# Для x86_64 кладутся настоящие бинарники. Остальные цели получают заглушки, которые
# печатают свою цель: их не запускают, а по выводу видят, какую сборку выбрал
# установщик. С `pad` к xray дописывается нулевой байт: файл отличается, версия та же.
set -euo pipefail

out=${1:?не указан каталог для ресурсов}
version=${2:?не указана версия}
raycat=${3:?не указан бинарник raycat}
xray=${4:?не указан бинарник xray}
pad=${5:-}

root=$(cd "$(dirname "$0")/../../.." && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$out"

pack() {
  local target=$1 real=$2
  local name="raycat-$version-$target"
  local dir="$stage/$name"
  mkdir -p "$dir/completions" "$dir/man" "$dir/systemd"
  if [ "$real" = yes ]; then
    install -m 755 "$raycat" "$dir/raycat"
    install -m 755 "$xray" "$dir/xray"
    if [ -n "$pad" ]; then
      printf '\0' >>"$dir/xray"
    fi
    "$raycat" completions bash >"$dir/completions/raycat.bash"
    "$raycat" completions zsh >"$dir/completions/_raycat"
    "$raycat" completions fish >"$dir/completions/raycat.fish"
    "$raycat" man >"$dir/man/raycat.1"
  else
    local tool
    for tool in raycat xray; do
      printf '#!/bin/sh\necho "%s %s"\n' "$tool" "$target" >"$dir/$tool"
      chmod 755 "$dir/$tool"
    done
    echo "# заглушка" | tee "$dir/completions/raycat.bash" "$dir/completions/_raycat" \
      "$dir/completions/raycat.fish" >"$dir/man/raycat.1"
  fi
  cp "$root/LICENSE" "$root/README.md" "$dir/"
  cp "$root/deploy/raycat.service" "$dir/systemd/raycat.service"
  tar -czf "$out/$name.tar.gz" -C "$stage" "$name"
}

pack x86_64-linux-musl yes
for target in aarch64-linux-musl aarch64-linux-musl-noaes armv7-linux-musleabihf armv7-linux-musleabihf-noaes; do
  pack "$target" no
done
(cd "$out" && sha256sum "raycat-$version"-*.tar.gz >SHA256SUMS)
ls -l "$out"
