#!/usr/bin/env bash
# Работает внутри reactivecircus/android-emulator-runner: ставит APK, открывает ссылку
# приложения «добавить подписку» на capture_server.py хоста, нажимает диалоги и
# сохраняет снимки экрана и дампы интерфейса рядом с пойманными запросами.
#   APK=Happ.apk SCHEME=happ HOSTS='10.0.2.2' OUT=captures bash android.sh
# CA_CERT и PROTO=https: приложение принимает подписку только по HTTPS (см. certs.sh).
# HOSTS: адреса сервера, которые пробуются по очереди до первого захвата (из эмулятора
# хост виден как 10.0.2.2; 127.0.0.1 и localhost работают через adb reverse).
set -u
out=${OUT:-captures}
port=${PORT:-18080}
mkdir -p "$out"
shot=0
snap() {
  shot=$((shot + 1))
  adb exec-out screencap -p >"$out/screen-$shot-$1.png" || true
  adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1 && adb pull /sdcard/ui.xml "$out/ui-$shot-$1.xml" >/dev/null 2>&1 || true
}
# Нажимает первый элемент, чей текст (без учёта регистра) совпал с одним из слов.
tap() {
  adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1 || return 1
  xml=$(adb exec-out cat /sdcard/ui.xml)
  for word in "$@"; do
    bounds=$(printf '%s' "$xml" | tr '>' '\n' | grep -i "text=\"$word\"" | grep -o 'bounds="[^"]*"' | head -1)
    [ -n "$bounds" ] || continue
    read -r x1 y1 x2 y2 <<<"$(printf '%s' "$bounds" | tr -c '0-9' ' ')"
    adb shell input tap $(((x1 + x2) / 2)) $(((y1 + y2) / 2))
    echo "нажато '$word'"
    return 0
  done
  return 1
}
captured() { ls "$out"/*.http >/dev/null 2>&1; }
# Ждёт первый запрос приложения, не дольше $1 секунд; после него ещё 3 с на повторные запросы.
wait_captured() {
  local deadline=$((SECONDS + $1))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if captured; then
      sleep 3
      return 0
    fi
    sleep 1
  done
  return 1
}
# Ждёт, пока на экране появится элемент с одним из текстов (или придёт запрос), не дольше $1 секунд.
# Дамп интерфейса в эмуляторе занимает секунды, поэтому предел считается по часам.
wait_ui() {
  local deadline=$((SECONDS + $1)) word
  shift
  while [ "$SECONDS" -lt "$deadline" ]; do
    captured && return 0
    adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
    xml=$(adb exec-out cat /sdcard/ui.xml)
    for word in "$@"; do
      printf '%s' "$xml" | grep -qi "text=\"$word\"" && return 0
    done
    sleep 1
  done
  return 1
}

# DATE=MMDDhhmmCCYY.ss переводит часы эмулятора (проверка значений, зависящих от дня).
if [ -n "${DATE:-}" ]; then
  adb root >/dev/null && adb wait-for-device
  adb shell settings put global auto_time 0
  adb shell date "$DATE"
fi
adb shell date -u | tee "$out/clock.txt"

aapt=$(find "$ANDROID_HOME/build-tools" -name aapt 2>/dev/null | sort -V | tail -1)
if [ -n "$aapt" ]; then
  "$aapt" dump badging "$APK" | grep -E "^package:|native-code|sdkVersion" | tee "$out/apk-info.txt"
  "$aapt" dump xmltree "$APK" AndroidManifest.xml >"$out/manifest.txt" 2>&1
  # Правила сети приложения: ресурс с сетевой конфигурацией может быть с любым именем.
  for xml in $(unzip -Z1 "$APK" 'res/*.xml'); do
    "$aapt" dump xmltree "$APK" "$xml" 2>/dev/null | grep -q network-security-config &&
      "$aapt" dump xmltree "$APK" "$xml" >>"$out/network-security.txt" 2>&1
  done
fi
pkg=$(grep -o "package: name='[^']*'" "$out/apk-info.txt" 2>/dev/null | cut -d"'" -f2)
if [ -n "${CA_CERT:-}" ]; then
  # Корневой ЦС попадает в системное хранилище: в Android 14 оно лежит в APEX, поэтому
  # каталог подменяется tmpfs и подключается в пространство имён zygote (новые процессы
  # приложений наследуют его).
  hash=$(openssl x509 -inform PEM -subject_hash_old -in "$CA_CERT" | head -1)
  adb root >/dev/null && adb wait-for-device
  adb push "$CA_CERT" "/data/local/tmp/$hash.0" >/dev/null
  adb shell "
    set -e
    mkdir -m 700 /data/local/tmp/ca-copy
    cp /apex/com.android.conscrypt/cacerts/* /data/local/tmp/ca-copy/
    mount -t tmpfs tmpfs /system/etc/security/cacerts
    cp /data/local/tmp/ca-copy/* /system/etc/security/cacerts/
    cp /data/local/tmp/$hash.0 /system/etc/security/cacerts/
    chown root:root /system/etc/security/cacerts/*
    chmod 644 /system/etc/security/cacerts/*
    chcon u:object_r:system_file:s0 /system/etc/security/cacerts/*
    for pid in 1 \$(pidof zygote) \$(pidof zygote64); do
      nsenter --mount=/proc/\$pid/ns/mnt -- /bin/mount --bind /system/etc/security/cacerts /apex/com.android.conscrypt/cacerts
    done
    ls /apex/com.android.conscrypt/cacerts | grep -c $hash
  " 2>&1 | tee "$out/ca-install.txt"
fi
adb install -r -g "$APK" 2>&1 | tail -2
adb reverse "tcp:$port" "tcp:$port" >/dev/null 2>&1
adb shell getprop ro.product.model >"$out/device.txt"
adb shell getprop ro.build.version.release >>"$out/device.txt"
adb shell settings get secure android_id >>"$out/device.txt"

[ -n "$pkg" ] && adb shell monkey -p "$pkg" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
wait_ui 15 "Wait" "Allow" "OK" "Accept" "Agree" "Continue" "Next" "Skip" "Got it" "Разрешить" "Принять" "Продолжить" "Далее" "Пропустить" || true
snap launched
for _ in 1 2 3 4 5 6; do
  tap "Wait" "Allow" "OK" "Accept" "Agree" "Continue" "Next" "Skip" "Got it" "Разрешить" "Принять" "Продолжить" "Далее" "Пропустить" || break
  sleep 3
done
snap onboarding

for host in $HOSTS; do
  captured && break
  adb shell am start -a android.intent.action.VIEW -d "$SCHEME://add/${PROTO:-http}://$host:$port/sub/capture-android" 2>&1 | tail -1
  wait_ui 12 "Wait" "Add" "OK" "Import" "Yes" "Confirm" "Добавить" "Импорт" "Да" "Подтвердить" || true
  snap "deeplink-$host"
  # Некоторые приложения добавляют подписку без диалога: тогда нажимать нечего.
  if ! captured; then
    for _ in 1 2 3; do
      tap "Wait" "Add" "OK" "Import" "Yes" "Confirm" "Добавить" "Импорт" "Да" "Подтвердить" || break
      if wait_captured 8; then
        break
      fi
    done
    snap "added-$host"
    wait_captured 15 || true
  fi
done
wait_captured 10 || true
snap final
[ -n "$pkg" ] && adb logcat -d --pid="$(adb shell pidof "$pkg" | awk '{print $1}')" >"$out/logcat-app.txt" 2>&1
ls -la "$out"
