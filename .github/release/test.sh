#!/usr/bin/env bash
# Тесты логики выпуска на выдуманных данных: версии, классификация и решение о продвижении,
# заметки, сборка и пересборка архивов, работа с git. Запуск: bash .github/release/test.sh
set -uo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$dir/lib.sh"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0

check() {
  if [ "$2" = "$3" ]; then
    echo "ok    $1"
  else
    echo "FAIL  $1"
    echo "  ожидалось: $2"
    echo "  получено:  $3"
    failures=$((failures + 1))
  fi
}

contains() {
  case $1 in
    *"$2"*) echo yes ;;
    *) echo no ;;
  esac
}

# Версии

version() {
  printf '%s\n' "$2" | bash "$dir/version.sh" "$1"
}

check "версия: feat поднимает minor" 0.1.0 "$(version 0.0.0 '{"subject":"feat: возможность"}')"
check "версия: fix поднимает patch" 0.0.1 "$(version 0.0.0 '{"subject":"fix: ошибка"}')"
check "версия: feat сильнее fix" 0.4.0 "$(version 0.3.1 $'{"subject":"fix: а"}\n{"subject":"feat(x): б"}')"
check "версия: до 1.0 breaking поднимает minor" 0.4.0 "$(version 0.3.1 '{"subject":"feat!: слом"}')"
check "версия: minor с двумя цифрами" 0.10.0 "$(version 0.9.9 '{"subject":"feat: а"}')"
check "версия: с 1.0 breaking в футере поднимает major" 2.0.0 "$(version 1.2.3 '{"subject":"fix: а","breaking":true}')"
check "версия: с 1.0 feat поднимает minor" 1.3.0 "$(version 1.2.3 '{"subject":"feat: а"}')"
check "версия: прочее поднимает patch" 1.2.4 "$(version 1.2.3 '{"subject":"docs: а"}')"
check "версия: без коммитов patch" 1.2.4 "$(version 1.2.3 '')"
if bash "$dir/version.sh" abc </dev/null >/dev/null 2>&1; then
  check "версия: неверная база отклоняется" error ok
else
  check "версия: неверная база отклоняется" error error
fi

# Решение о продвижении

DAY=86400
NOW=1000000
SRC='["crates/raycat/src/main.rs"]'
EMU='["crates/emulation/profiles/happ.toml"]'

cm() {
  jq -cn --arg sha "$1" --arg subject "$2" --argjson files "$3" --argjson labels "${4:-[]}" --argjson breaking "${5:-false}" \
    '{sha: $sha, subject: $subject, files: $files, labels: $labels, breaking: $breaking}'
}

bd() {
  jq -cn --arg tag "$1" --arg sha "$2" --argjson at "$((NOW - $3))" '{tag: $tag, sha: $sha, published_at: $at}'
}

plan() {
  bash "$dir/plan.sh" --commits "$work/commits.jsonl" --builds "$work/builds.jsonl" --now "$NOW" "$@"
}

summary() {
  jq -r '"\(.promote) \(.tag) \(.version)"'
}

{
  cm c1 "feat: возможность" "$SRC"
  cm c2 "fix(emulation): профиль" "$EMU"
} >"$work/commits.jsonl"
{
  bd v0.2.0-dev.1 c1 $((4 * DAY))
  bd v0.2.0-dev.2 c2 $((2 * 3600))
} >"$work/builds.jsonl"
check "auto: свежая эмуляция ждёт, берётся прошлая сборка" "true v0.2.0-dev.1 0.2.0" "$(plan --last 0.1.0 | summary)"

{
  bd v0.2.0-dev.1 c1 $((4 * DAY))
  bd v0.2.0-dev.2 c2 $((25 * 3600))
} >"$work/builds.jsonl"
check "auto: эмуляция выдержана за 24 часа" "true v0.2.0-dev.2 0.2.0" "$(plan --last 0.1.0 | summary)"
check "стоп-релиз останавливает продвижение" false "$(plan --last 0.1.0 --stop | jq -r .promote)"
check "стоп-релиз останавливает и ручной запуск" false "$(plan --last 0.1.0 --stop --mode manual --requested v0.2.0-dev.2 --ignore-wait | jq -r .promote)"

{
  cm c1 "feat: возможность" "$SRC"
  cm c2 "fix: ошибка" "$SRC"
} >"$work/commits.jsonl"
{
  bd v0.2.0-dev.1 c1 $((4 * DAY))
  bd v0.2.0-dev.2 c2 $((2 * DAY))
} >"$work/builds.jsonl"
check "auto: код ждёт 3 суток" "true v0.2.0-dev.1 0.2.0" "$(plan --last 0.1.0 | summary)"

{
  bd v0.2.0-dev.2 c2 $((4 * DAY))
} >"$work/builds.jsonl"
check "auto: срок считается от первой сборки с изменением" "true v0.2.0-dev.2 0.2.0" "$(plan --last 0.1.0 | summary)"

{
  cm c1 "feat: возможность" "$SRC"
  cm c2 "fix: ошибка" "$SRC"
  cm c3 "fix: ещё ошибка" "$SRC"
} >"$work/commits.jsonl"
check "auto: коммит без сборки не мешает" 3 "$(plan --last 0.1.0 | jq -r '.entries | length')"
check "auto: коммит без сборки не входит в выпуск" "true v0.2.0-dev.2 0.2.0" "$(plan --last 0.1.0 | summary)"

: >"$work/builds.jsonl"
check "auto: без dev-сборок продвигать нечего" false "$(plan --last 0.1.0 | jq -r .promote)"

{
  cm c1 "feat: возможность" "$SRC"
  cm c2 "fix: ошибка" '["versions.toml"]'
} >"$work/commits.jsonl"
{
  bd v0.2.0-dev.1 c1 $((4 * DAY))
  bd v0.2.0-dev.2 c2 $((2 * DAY))
} >"$work/builds.jsonl"
check "auto: смена версии xray ждёт 3 суток" "true v0.2.0-dev.1 0.2.0" "$(plan --last 0.1.0 | summary)"

{
  cm c1 "feat: возможность" "$SRC"
  cm c2 "fix: профиль и код" '["crates/emulation/profiles/happ.toml","crates/raycat/src/main.rs"]'
} >"$work/commits.jsonl"
check "auto: эмуляция вместе с кодом ждёт 3 суток" "true v0.2.0-dev.1 0.2.0" "$(plan --last 0.1.0 | summary)"

{
  cm c1 "feat!: слом совместимости" "$SRC"
} >"$work/commits.jsonl"
{
  bd v0.2.0-dev.1 c1 $((4 * DAY))
} >"$work/builds.jsonl"
check "auto: breaking только вручную" false "$(plan --last 0.1.0 | jq -r .promote)"
check "auto: причина названа" yes "$(contains "$(plan --last 0.1.0 | jq -r .reason)" "только вручную")"
check "ручной запуск: breaking до 1.0 даёт minor" "true v0.2.0-dev.1 0.2.0" "$(plan --last 0.1.0 --mode manual --requested v0.2.0-dev.1 | summary)"
check "ручной запуск: нет такой сборки" false "$(plan --last 0.1.0 --mode manual --requested v9.9.9-dev.9 | jq -r .promote)"

{
  bd v0.2.0-dev.1 c1 3600
} >"$work/builds.jsonl"
check "ручной запуск: срок соблюдается" false "$(plan --last 0.1.0 --mode manual --requested v0.2.0-dev.1 | jq -r .promote)"
check "ручной запуск: срок можно игнорировать" "true v0.2.0-dev.1 0.2.0" "$(plan --last 0.1.0 --mode manual --requested v0.2.0-dev.1 --ignore-wait | summary)"

{
  bd v2.0.0-dev.1 c1 3600
} >"$work/builds.jsonl"
check "мажорная версия: breaking с 1.0, вручную" "true v2.0.0-dev.1 2.0.0" "$(plan --last 1.0.0 --mode manual --requested v2.0.0-dev.1 --ignore-wait | summary)"
check "мажорная версия: автоматически нельзя" false "$(plan --last 1.0.0 | jq -r .promote)"

{
  cm c1 "refactor: переработка" "$SRC"
  cm c2 "fix: закрыта уязвимость" "$SRC" '["безопасность"]'
  cm c3 "feat: возможность" "$SRC"
} >"$work/commits.jsonl"
{
  bd v0.1.1-dev.1 c1 3600
  bd v0.1.1-dev.2 c2 3000
  bd v0.2.0-dev.3 c3 600
} >"$work/builds.jsonl"
check "hotfix: уходит сразу, без последующих изменений" "true v0.1.1-dev.2 0.1.1" "$(plan --last 0.1.0 | summary)"

{
  cm c1 "refactor: переработка" "$SRC"
  cm c2 "fix: ошибка" "$SRC"
} >"$work/commits.jsonl"
{
  bd v0.1.1-dev.1 c1 3600
  bd v0.1.1-dev.2 c2 3000
} >"$work/builds.jsonl"
check "hotfix: без метки не срочный" false "$(plan --last 0.1.0 | jq -r .promote)"

{
  cm c1 "refactor: переработка" "$SRC"
  cm c2 "docs: описание уязвимости" "$SRC" '["безопасность"]'
} >"$work/commits.jsonl"
check "hotfix: нужен заголовок fix" false "$(plan --last 0.1.0 | jq -r .promote)"

{
  cm c1 "feat!: слом" "$SRC" '[]' true
  cm c2 "fix: закрыта уязвимость" "$SRC" '["безопасность"]'
} >"$work/commits.jsonl"
check "hotfix не обходит breaking" false "$(plan --last 0.1.0 | jq -r .promote)"

: >"$work/commits.jsonl"
check "нет изменений: продвигать нечего" false "$(plan --last 0.1.0 | jq -r .promote)"

{
  cm c1 "chore: служебное" '["versions.toml"]'
  cm c2 "fix: профиль" "$EMU"
  cm c3 "fix: профиль и захват" '["crates/emulation/profiles/a.toml","crates/emulation/captures/a.http"]'
  cm c4 "fix: профиль и код" '["crates/emulation/profiles/a.toml","crates/raycat/src/main.rs"]'
  cm c5 "chore: пустой коммит" '[]'
  cm c6 "feat!: слом" "$SRC"
  cm c7 "fix: уязвимость" "$SRC" '["безопасность"]'
  cm c8 "chore: конфиг" "$SRC" '[]' true
} >"$work/commits.jsonl"
check "классы коммитов" "xray,emulation,emulation,code,code,breaking,hotfix,breaking" "$(plan --last 0.1.0 | jq -r '[.entries[].category] | join(",")')"
check "сроки коммитов, ч" "72,24,24,72,72,72,0,72" "$(plan --last 0.1.0 | jq -r '[.entries[].wait / 3600] | join(",")')"

# Заметки

{
  cm c1 "feat(gateway): шлюз для LAN (#5)" "$SRC"
  cm c2 "fix: утечка DNS & кэш (#6)" "$SRC"
  cm c3 "chore(deps): обновить serde с 1.0.1 до 1.0.2 (#9)" "$SRC"
  cm c4 "feat: второй вариант (#7)" "$SRC"
  cm c5 "feat!: слом (#10)" "$SRC"
  cm c6 "Слитый коммит без типа" "$SRC"
} >"$work/commits.jsonl"
notes=$(bash "$dir/notes.sh" <"$work/commits.jsonl")
check "заметки: группа новых возможностей" yes "$(contains "$notes" "### Новое")"
check "заметки: область перед описанием" yes "$(contains "$notes" "- gateway: шлюз для LAN (#5)")"
check "заметки: новые выше старых" "- второй вариант (#7)" "$(grep -A4 '### Новое' <<<"$notes" | sed -n 3p)"
check "заметки: исправления" yes "$(contains "$notes" "- утечка DNS & кэш (#6)")"
check "заметки: несовместимые изменения" yes "$(contains "$notes" "### Несовместимые изменения")"
check "заметки: служебное" yes "$(contains "$notes" "- deps: обновить serde с 1.0.1 до 1.0.2 (#9)")"
check "заметки: заголовок без типа остаётся" yes "$(contains "$notes" "- Слитый коммит без типа")"
check "заметки: тип коммита не торчит" no "$(contains "$notes" "feat:")"

dev_notes=$(printf '%s\n' "$notes" | bash "$dir/make-notes.sh" dev 0.2.0-dev.7 raycat-app/raycat v0.1.0)
check "заметки dev: образ" yes "$(contains "$dev_notes" "docker pull ghcr.io/raycat-app/raycat:0.2.0-dev.7")"
check "заметки dev: с какого тега" yes "$(contains "$dev_notes" "## Изменения с v0.1.0")"
check "заметки dev: символ & не ломается" yes "$(contains "$dev_notes" "- утечка DNS & кэш (#6)")"
check "заметки dev: проверка аттестации" yes "$(contains "$dev_notes" "gh attestation verify oci://ghcr.io/raycat-app/raycat:0.2.0-dev.7 --repo raycat-app/raycat")"
check "заметки dev: плейсхолдеров не осталось" no "$(contains "$dev_notes" "@")"
stable_notes=$(printf '%s\n' "- а" | bash "$dir/make-notes.sh" stable 0.2.0 raycat-app/raycat "" 0.2.0-dev.7)
check "заметки stable: короткий тег минорной версии" yes "$(contains "$stable_notes" ':0.2`')"
check "заметки stable: исходная dev-сборка" yes "$(contains "$stable_notes" "0.2.0-dev.7")"
check "заметки stable: без тега начала нет «с»" yes "$(contains "$stable_notes" "## Изменения
")"
check "заметки stable: плейсхолдеров не осталось" no "$(contains "$stable_notes" "@")"
check "заметки: пустой список" yes "$(contains "$(printf '' | bash "$dir/make-notes.sh" dev 0.2.0-dev.7 raycat-app/raycat)" "Изменений нет.")"

# git: теги, диапазоны, версия dev

repo="$work/repo"
git init -q "$repo"
g() {
  git -C "$repo" -c user.name=test -c user.email=test@example.com -c commit.gpgsign=false "$@"
}
mkdir -p "$repo/crates/emulation/profiles"
echo a >"$repo/a.txt"
g add -A
g commit -q -m "feat: первый"
g tag v0.1.0
g tag v0.9.0
echo b >"$repo/crates/emulation/profiles/p.toml"
g add -A
g commit -q -m "fix: профиль" -m "BREAKING CHANGE: меняется формат"
g tag v0.10.0
g tag v0.10.1-dev.2
echo c >"$repo/c.txt"
g add -A
g commit -q -m "feat: третий"

in_repo() {
  (cd "$repo" && "$@")
}

check "stable-тег: версии сравниваются по числам, dev не считается" v0.10.0 "$(in_repo bash "$dir/stable-tag.sh" HEAD)"
check "stable-тег: только достижимые" v0.9.0 "$(in_repo bash "$dir/stable-tag.sh" HEAD~2)"
repo_dev_only="$work/repo-dev-only"
git init -q "$repo_dev_only"
echo x >"$repo_dev_only/x"
git -C "$repo_dev_only" add -A
git -C "$repo_dev_only" -c user.name=test -c user.email=test@example.com -c commit.gpgsign=false commit -q -m "feat: х"
git -C "$repo_dev_only" tag v0.1.0-dev.1
check "stable-тег: только dev-теги — пусто" "" "$(cd "$repo_dev_only" && bash "$dir/stable-tag.sh" HEAD)"
check "collect: число коммитов диапазона" 2 "$(in_repo bash "$dir/collect.sh" v0.9.0 HEAD | wc -l | tr -d ' ')"
check "collect: вся история без базы" 3 "$(in_repo bash "$dir/collect.sh" "" HEAD | wc -l | tr -d ' ')"
check "collect: файлы и футер breaking" "fix: профиль true crates/emulation/profiles/p.toml" \
  "$(in_repo bash "$dir/collect.sh" v0.9.0 HEAD | head -n 1 | jq -r '[.subject, .breaking, (.files | join(" "))] | join(" ")')"
check "collect: файлы корневого коммита" "a.txt" "$(in_repo bash "$dir/collect.sh" "" HEAD | head -n 1 | jq -r '.files | join(" ")')"
check "collect: коммит без футера" false "$(in_repo bash "$dir/collect.sh" v0.10.0 HEAD | head -n 1 | jq -r .breaking)"

: >"$work/gh-output"
in_repo env RUN_NUMBER=57 GITHUB_OUTPUT="$work/gh-output" bash "$dir/dev-version.sh" 2>/dev/null
check "dev-версия: следующая stable и номер запуска" "version=0.11.0-dev.57" "$(grep '^version=' "$work/gh-output")"
check "dev-версия: тег" "tag=v0.11.0-dev.57" "$(grep '^tag=' "$work/gh-output")"
check "dev-версия: прошлая stable" "last_tag=v0.10.0" "$(grep '^last_tag=' "$work/gh-output")"
check "dev-версия: прошлая dev" "previous=v0.10.1-dev.2" "$(grep '^previous=' "$work/gh-output")"

# Архивы

inputs="$work/inputs"
mkdir -p "$inputs"
for name in raycat-x86_64 raycat-aarch64 raycat-armv7 xray-x86_64 xray-aarch64 xray-aarch64-noaes xray-armv7 xray-armv7-noaes \
  LICENSE README.md raycat.bash _raycat raycat.fish raycat.1 raycat.service; do
  echo "$name" >"$inputs/$name"
done

export SOURCE_DATE_EPOCH=1700000000
bash "$dir/package.sh" 0.2.0-dev.7 "$inputs" "$work/dev-a"
bash "$dir/package.sh" 0.2.0-dev.7 "$inputs" "$work/dev-b"
check "архивы: воспроизводимы" "$(cat "$work/dev-a/SHA256SUMS")" "$(cat "$work/dev-b/SHA256SUMS")"
check "архивы: пять архивов и суммы" 6 "$(find "$work/dev-a" -type f | wc -l | tr -d ' ')"
check "архивы: суммы сходятся" ok "$(cd "$work/dev-a" && sha256sum -c SHA256SUMS >/dev/null && echo ok)"

name=raycat-0.2.0-dev.7-armv7-linux-musleabihf-noaes
expected_members=$(
  printf '%s\n' "$name/" "$name/LICENSE" "$name/README.md" "$name/completions/" "$name/completions/_raycat" \
    "$name/completions/raycat.bash" "$name/completions/raycat.fish" "$name/man/" "$name/man/raycat.1" "$name/raycat" \
    "$name/systemd/" "$name/systemd/raycat.service" "$name/xray" | LC_ALL=C sort
)
check "архивы: состав" "$expected_members" "$(tar -tzf "$work/dev-a/$name.tar.gz" | LC_ALL=C sort)"

mkdir -p "$work/x-plain" "$work/x-noaes"
tar -xzf "$work/dev-a/raycat-0.2.0-dev.7-armv7-linux-musleabihf.tar.gz" -C "$work/x-plain"
tar -xzf "$work/dev-a/$name.tar.gz" -C "$work/x-noaes"
archive_diff=$(diff -rq "$work/x-plain/raycat-0.2.0-dev.7-armv7-linux-musleabihf" "$work/x-noaes/$name")
check "архивы: noaes отличается одним файлом" 1 "$(printf '%s\n' "$archive_diff" | wc -l | tr -d ' ')"
check "архивы: noaes отличается только xray" yes "$(contains "$archive_diff" "/xray and ")"
check "архивы: noaes берёт xray без AES" "xray-armv7-noaes" "$(cat "$work/x-noaes/$name/xray")"
check "архивы: бинарник исполняемый" 755 "$(stat -c %a "$work/x-noaes/$name/raycat")"
check "архивы: служебные файлы без прав на исполнение" 644 "$(stat -c %a "$work/x-noaes/$name/systemd/raycat.service")"

bash "$dir/repack.sh" 0.2.0-dev.7 0.2.0 "$work/dev-a" "$work/stable"
check "stable: имена архивов" "$(printf 'raycat-0.2.0-%s.tar.gz\n' "${ARCHIVE_SUFFIXES[@]}" | LC_ALL=C sort | paste -sd ' ')" \
  "$(cd "$work/stable" && ls raycat-*.tar.gz | LC_ALL=C sort | paste -sd ' ')"
check "stable: суммы сходятся" ok "$(cd "$work/stable" && sha256sum -c SHA256SUMS >/dev/null && echo ok)"
mkdir -p "$work/x-stable"
tar -xzf "$work/stable/raycat-0.2.0-x86_64-linux-musl.tar.gz" -C "$work/x-stable"
mkdir -p "$work/x-dev"
tar -xzf "$work/dev-a/raycat-0.2.0-dev.7-x86_64-linux-musl.tar.gz" -C "$work/x-dev"
check "stable: файлы те же, что в dev" "$(fingerprint "$work/x-dev/raycat-0.2.0-dev.7-x86_64-linux-musl")" \
  "$(fingerprint "$work/x-stable/raycat-0.2.0-x86_64-linux-musl")"
check "stable: каталог внутри архива переименован" raycat-0.2.0-x86_64-linux-musl "$(ls "$work/x-stable")"

rm "$work/dev-a/raycat-0.2.0-dev.7-armv7-linux-musleabihf.tar.gz"
if bash "$dir/repack.sh" 0.2.0-dev.7 0.2.0 "$work/dev-a" "$work/stable-broken" 2>/dev/null; then
  check "stable: нет архива — ошибка" error ok
else
  check "stable: нет архива — ошибка" error error
fi

# Проверка набора файлов dev-выпуска (без сети)

bash "$dir/package.sh" 0.2.0-dev.7 "$inputs" "$work/dev-c"
check "проверка dev-выпуска: полный набор" ok "$(SKIP_ATTESTATION=1 bash "$dir/verify-dev.sh" "$work/dev-c" 0.2.0-dev.7 >/dev/null 2>&1 && echo ok)"
echo лишнее >"$work/dev-c/extra.txt"
check "проверка dev-выпуска: лишний файл" error "$(SKIP_ATTESTATION=1 bash "$dir/verify-dev.sh" "$work/dev-c" 0.2.0-dev.7 >/dev/null 2>&1 && echo ok || echo error)"
rm "$work/dev-c/extra.txt"
echo повреждено >>"$work/dev-c/raycat-0.2.0-dev.7-x86_64-linux-musl.tar.gz"
check "проверка dev-выпуска: испорченный архив" error "$(SKIP_ATTESTATION=1 bash "$dir/verify-dev.sh" "$work/dev-c" 0.2.0-dev.7 >/dev/null 2>&1 && echo ok || echo error)"

if [ "$failures" -ne 0 ]; then
  echo "Провалено проверок: $failures"
  exit 1
fi
echo "Все проверки пройдены"
