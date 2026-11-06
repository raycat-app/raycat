#!/usr/bin/env bash
# Заметки к выпуску на русском. Список изменений (вывод notes.sh) читается из stdin.
#   make-notes.sh dev <версия> <владелец/репозиторий> [<с какого тега изменения>]
#   make-notes.sh stable <версия> <владелец/репозиторий> <с какого тега изменения или пусто> <версия dev-сборки>
#   make-notes.sh xray <версия> <владелец/репозиторий> <прошлая stable> <коммит main с .github/versions.toml>
#
# Вид как у README проекта: изменения в раскрывающемся блоке, каждая группа («Новое»,
# «Исправления»…) свёрнута отдельно и показывает число пунктов; установка — команды и
# таблица на всю ширину (страница выпуска шире README, поэтому колонки с запасом);
# проверка подлинности свёрнута.
set -euo pipefail

kind=${1:?не указан вид выпуска}
version=${2:?не указана версия}
repo=${3:?не указан репозиторий}
since=${4-}
extra=${5-}

changes=$(cat)
if [ -z "$changes" ]; then
  changes="Изменений нет."
fi

# Группы «### Название» из notes.sh — в свёрнутые блоки «Название · число пунктов».
fold_groups() {
  awk '
    { line[NR] = $0 }
    END {
      groups = 0
      for (i = 1; i <= NR; i++) {
        if (line[i] ~ /^### /) { groups++; title[groups] = substr(line[i], 5) }
        else if (groups > 0 && line[i] ~ /^- /) count[groups]++
      }
      group = 0
      for (i = 1; i <= NR; i++) {
        if (line[i] ~ /^### /) {
          if (group > 0) print "\n</details>\n"
          group++
          print "<details>"
          print "<summary>" title[group] " · " count[group] "</summary>"
          continue
        }
        print line[i]
      }
      if (group > 0) print "\n</details>"
    }'
}

IFS= read -r -d '' changes_block <<'EOF' || true
<details open>
<summary><b>Изменения@SINCE@</b></summary>

@CHANGES@

</details>
EOF

IFS= read -r -d '' install_block <<'EOF' || true
## Установка

```sh
docker pull @IMAGE@:@VERSION@
curl -fsSL https://raw.githubusercontent.com/@REPO@/main/deploy/install.sh | sudo sh -s -- --version @VERSION@
```

<table>
<tr><th width="500">Что</th><th width="1500">Где взять</th></tr>
<tr><td><b>Образ Docker</b></td><td><code>@IMAGE@:@VERSION@</code>@ALSO@</td></tr>
<tr><td><b>Образ без аппаратного AES</b></td><td><code>@IMAGE@:@VERSION@-noaes</code>@ALSO_NOAES@ — для Raspberry Pi 3 и 4 и других процессоров без AES</td></tr>
<tr><td><b>Архивы</b></td><td>файлы выпуска ниже: x86_64, aarch64 и armv7; для процессоров без AES — с <code>-noaes</code> в имени</td></tr>@NOAES_ROW@
</table>

<details>
<summary><b>Проверка подлинности</b></summary>

```sh
sha256sum -c SHA256SUMS
gh attestation verify raycat-@VERSION@-x86_64-linux-musl.tar.gz --repo @REPO@
gh attestation verify oci://@IMAGE@:@VERSION@ --repo @REPO@
```

</details>
EOF

case $kind in
  dev)
    intro="Предварительная сборка канала dev: её ставят на канарейку до выхода в stable. Для рабочих серверов берите stable."
    template="$intro"$'\n\n'"$changes_block"$'\n'"$install_block"
    also=", также <code>:dev</code>"
    also_noaes=", также <code>:dev-noaes</code>"
    ;;
  xray)
    [ -n "$since" ] && [ -n "$extra" ] || {
      echo "ОШИБКА: для xray нужны прошлая stable и коммит main" >&2
      exit 1
    }
    intro="Обновление xray до @XRAY@. Сам raycat не менялся: его файлы те же, что в @PREVIOUS@, заменён только xray."
    intro+=$'\n\n'"Версия xray взята из .github/versions.toml коммита @COMMIT@. Релиз xray: https://github.com/XTLS/Xray-core/releases/tag/v@XRAY@"
    template="$intro"$'\n\n'"$install_block"
    also=", также <code>:latest</code> и <code>:@MINOR@</code>"
    also_noaes=", также <code>:latest-noaes</code>"
    ;;
  stable)
    [ -n "$extra" ] || {
      echo "ОШИБКА: для stable нужна версия dev-сборки" >&2
      exit 1
    }
    intro="Стабильный выпуск. Это та же сборка, что прошла канал dev (\`@DEV_VERSION@\`) и выдержала срок: файлы и образы не пересобирались."
    template="$intro"$'\n\n'"$changes_block"$'\n'"$install_block"
    also=", также <code>:latest</code> и <code>:@MINOR@</code>"
    also_noaes=", также <code>:latest-noaes</code>"
    ;;
  *)
    echo "ОШИБКА: вид выпуска: dev, stable или xray" >&2
    exit 1
    ;;
esac

since_text=
if [ -n "$since" ]; then
  since_text=" с $since"
fi
image="ghcr.io/${repo,,}"
minor=${version%.*}

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
xray_version=$(sed -n '/^\[xray\]$/,/^\[/ s/^version = "\(.*\)"/\1/p' "$dir/../versions.toml")
noaes_version=$(sed -n '/^\[xray\.noaes\]$/,/^\[/ s/^version = "\(.*\)"/\1/p' "$dir/../versions.toml")
noaes_row=
if [ "$noaes_version" != "$xray_version" ]; then
  noaes_row=$'\n'"<tr><td><b>Версия xray</b></td><td>основная сборка на xray $xray_version, сборка <code>-noaes</code> — на xray $noaes_version</td></tr>"
fi

folded=$(printf '%s\n' "$changes" | fold_groups)

# Замены в кавычках: иначе «&» в заголовке коммита bash 5.2 подставит как совпадение.
# Сначала то, что само содержит плейсхолдеры (@MINOR@ в also), потом остальное.
text=${template//@ALSO@/"$also"}
text=${text//@ALSO_NOAES@/"$also_noaes"}
text=${text//@NOAES_ROW@/"$noaes_row"}
text=${text//@SINCE@/"$since_text"}
text=${text//@VERSION@/"$version"}
text=${text//@MINOR@/"$minor"}
text=${text//@DEV_VERSION@/"$extra"}
text=${text//@XRAY@/"$xray_version"}
text=${text//@PREVIOUS@/"$since"}
text=${text//@COMMIT@/"${extra:0:7}"}
text=${text//@IMAGE@/"$image"}
text=${text//@REPO@/"$repo"}
text=${text//@CHANGES@/"$folded"}
printf '%s\n' "$text"
