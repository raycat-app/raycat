#!/usr/bin/env bash
# Заметки к выпуску на русском. Список изменений (вывод notes.sh) читается из stdin.
#   make-notes.sh dev <версия> <владелец/репозиторий> [<с какого тега изменения>]
#   make-notes.sh stable <версия> <владелец/репозиторий> <с какого тега изменения или пусто> <версия dev-сборки>
set -euo pipefail

kind=${1:?не указан вид выпуска}
version=${2:?не указана версия}
repo=${3:?не указан репозиторий}
since=${4-}
dev_version=${5-}

changes=$(cat)
if [ -z "$changes" ]; then
  changes="Изменений нет."
fi

case $kind in
  dev)
    IFS= read -r -d '' template <<'EOF' || true
Предварительная сборка канала dev: её ставят на канарейку до выхода в stable. Для рабочих серверов берите stable.

## Изменения@SINCE@

@CHANGES@

## Установка

Образ: `docker pull @IMAGE@:@VERSION@`. Для Raspberry Pi 3/4 и других процессоров без аппаратного AES: `@IMAGE@:@VERSION@-noaes`.

Архивы для x86_64, aarch64 и armv7 лежат в ресурсах выпуска. Для ARM-процессоров без AES берите архивы с `-noaes` в имени.

## Проверка подлинности

```sh
sha256sum -c SHA256SUMS
gh attestation verify raycat-@VERSION@-x86_64-linux-musl.tar.gz --repo @REPO@
gh attestation verify oci://@IMAGE@:@VERSION@ --repo @REPO@
```
EOF
    ;;
  stable)
    [ -n "$dev_version" ] || {
      echo "ОШИБКА: для stable нужна версия dev-сборки" >&2
      exit 1
    }
    IFS= read -r -d '' template <<'EOF' || true
Стабильный выпуск. Это та же сборка, что прошла канал dev (`@DEV_VERSION@`) и выдержала срок: файлы и образы не пересобирались.

## Изменения@SINCE@

@CHANGES@

## Установка

Образ: `docker pull @IMAGE@:@VERSION@` (также `:latest` и `:@MINOR@`). Для Raspberry Pi 3/4 и других процессоров без аппаратного AES: `@IMAGE@:@VERSION@-noaes` (также `:latest-noaes`).

Архивы для x86_64, aarch64 и armv7 лежат в ресурсах выпуска. Для ARM-процессоров без AES берите архивы с `-noaes` в имени.

## Проверка подлинности

```sh
sha256sum -c SHA256SUMS
gh attestation verify raycat-@VERSION@-x86_64-linux-musl.tar.gz --repo @REPO@
gh attestation verify oci://@IMAGE@:@VERSION@ --repo @REPO@
```
EOF
    ;;
  *)
    echo "ОШИБКА: вид выпуска: dev или stable" >&2
    exit 1
    ;;
esac

since_text=
if [ -n "$since" ]; then
  since_text=" с $since"
fi
image="ghcr.io/${repo,,}"
minor=${version%.*}

# Замены в кавычках: иначе «&» в заголовке коммита bash 5.2 подставит как совпадение.
text=${template//@SINCE@/"$since_text"}
text=${text//@VERSION@/"$version"}
text=${text//@MINOR@/"$minor"}
text=${text//@DEV_VERSION@/"$dev_version"}
text=${text//@IMAGE@/"$image"}
text=${text//@REPO@/"$repo"}
text=${text//@CHANGES@/"$changes"}
printf '%s\n' "$text"
