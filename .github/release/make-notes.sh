#!/usr/bin/env bash
# Заметки к выпуску на русском. Список изменений (вывод notes.sh) читается из stdin.
#   make-notes.sh dev <версия> <владелец/репозиторий> [<с какого тега изменения>]
#   make-notes.sh stable <версия> <владелец/репозиторий> <с какого тега изменения или пусто> <версия dev-сборки>
#   make-notes.sh xray <версия> <владелец/репозиторий> <прошлая stable> <коммит main с .github/versions.toml>
#
# Вид: одна строка о выпуске, затем разделы «Изменения», «Установка» и «Проверка
# подлинности» — заголовки со стрелкой (<summary><h2>), первые два открыты. Группы
# изменений («Новое», «Исправления»…) — свёрнутые заголовки поменьше с числом пунктов.
# Установка — один блок команд с подписями.
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

# Группы «### Название» из notes.sh — в свёрнутые заголовки «Название · число пунктов».
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
          print "<summary><h3>" title[group] " · " count[group] "</h3></summary>"
          continue
        }
        print line[i]
      }
      if (group > 0) print "\n</details>"
    }'
}

IFS= read -r -d '' changes_block <<'EOF' || true
<details open>
<summary><h2>Изменения@SINCE@</h2></summary>

@CHANGES@

</details>
EOF

IFS= read -r -d '' install_block <<'EOF' || true
<details open>
<summary><h2>Установка</h2></summary>

```sh
# Docker (также @ALSO@)
docker pull @IMAGE@:@VERSION@

# Docker для процессоров без AES, например Raspberry Pi 3 и 4 (также @ALSO_NOAES@)
docker pull @IMAGE@:@VERSION@-noaes

# служба на сервере: установщик сам выберет архив под процессор
curl -fsSL https://raw.githubusercontent.com/@REPO@/main/deploy/install.sh | sudo sh -s -- --version @VERSION@
```

Архивы для x86_64, aarch64 и armv7 — в файлах выпуска ниже, для процессоров без AES — с `-noaes` в имени.@NOAES_NOTE@

</details>

<details>
<summary><h2>Проверка подлинности</h2></summary>

```sh
sha256sum -c SHA256SUMS
gh attestation verify raycat-@VERSION@-x86_64-linux-musl.tar.gz --repo @REPO@
gh attestation verify oci://@IMAGE@:@VERSION@ --repo @REPO@
```

</details>
EOF

case $kind in
  dev)
    intro="Предварительная сборка канала dev для проверки перед stable. Для рабочих серверов берите stable."
    template="$intro"$'\n\n'"$changes_block"$'\n'"$install_block"
    also=":dev"
    also_noaes=":dev-noaes"
    ;;
  xray)
    [ -n "$since" ] && [ -n "$extra" ] || {
      echo "ОШИБКА: для xray нужны прошлая stable и коммит main" >&2
      exit 1
    }
    intro="Обновление xray до @XRAY@. Сам raycat не менялся: его файлы те же, что в @PREVIOUS@, заменён только xray."
    intro+=$'\n\n'"Версия xray взята из .github/versions.toml коммита @COMMIT@. Релиз xray: https://github.com/XTLS/Xray-core/releases/tag/v@XRAY@"
    template="$intro"$'\n\n'"$install_block"
    also=":latest и :@MINOR@"
    also_noaes=":latest-noaes"
    ;;
  stable)
    [ -n "$extra" ] || {
      echo "ОШИБКА: для stable нужна версия dev-сборки" >&2
      exit 1
    }
    intro="Стабильная версия @VERSION@ — та же сборка, что прошла канал dev как \`@DEV_VERSION@\`."
    template="$intro"$'\n\n'"$changes_block"$'\n'"$install_block"
    also=":latest и :@MINOR@"
    also_noaes=":latest-noaes"
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
noaes_note=
if [ "$noaes_version" != "$xray_version" ]; then
  noaes_note=" Сборка \`-noaes\` собрана на xray $noaes_version, основная — на xray $xray_version."
fi

folded=$(printf '%s\n' "$changes" | fold_groups)

# Замены в кавычках: иначе «&» в заголовке коммита bash 5.2 подставит как совпадение.
# Сначала то, что само содержит плейсхолдеры (@MINOR@ в also), потом остальное.
text=${template//@ALSO@/"$also"}
text=${text//@ALSO_NOAES@/"$also_noaes"}
text=${text//@NOAES_NOTE@/"$noaes_note"}
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
