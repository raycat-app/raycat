#!/usr/bin/env bash
# Работает внутри reactivecircus/android-emulator-runner: ставит APK, открывает ссылку
# приложения «добавить подписку» на capture_server.py хоста (из эмулятора он виден как
# 10.0.2.2), нажимает диалоги и сохраняет снимки экрана и дампы интерфейса рядом с
# пойманными запросами.
#   APK=Happ.apk LINK='happ://add/http://10.0.2.2:18080/sub/abc' OUT=captures bash android.sh
set -u
out=${OUT:-captures}
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

# DATE=MMDDhhmmCCYY.ss переводит часы эмулятора (проверка значений, зависящих от дня).
if [ -n "${DATE:-}" ]; then
  adb root >/dev/null && adb wait-for-device
  adb shell settings put global auto_time 0
  adb shell date "$DATE"
fi
adb shell date -u | tee "$out/clock.txt"

aapt=$(find "$ANDROID_HOME/build-tools" -name aapt 2>/dev/null | sort -V | tail -1)
[ -n "$aapt" ] && "$aapt" dump badging "$APK" | grep -E "^package:|native-code|sdkVersion" | tee "$out/apk-info.txt"
pkg=$(grep -o "package: name='[^']*'" "$out/apk-info.txt" 2>/dev/null | cut -d"'" -f2)
[ -n "$aapt" ] && "$aapt" dump xmltree "$APK" AndroidManifest.xml >"$out/manifest.txt" 2>&1
adb install -r -g "$APK" 2>&1 | tail -2
adb shell getprop ro.product.model >"$out/device.txt"
adb shell getprop ro.build.version.release >>"$out/device.txt"
adb shell settings get secure android_id >>"$out/device.txt"

[ -n "$pkg" ] && adb shell monkey -p "$pkg" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
sleep 15
snap launched
for _ in 1 2 3 4 5 6; do
  tap "Allow" "OK" "Accept" "Agree" "Continue" "Next" "Skip" "Got it" "Разрешить" "Принять" "Продолжить" "Далее" "Пропустить" || break
  sleep 3
done
snap onboarding

captured() { ls "$out"/*.http >/dev/null 2>&1; }

adb shell am start -a android.intent.action.VIEW -d "$LINK" 2>&1 | tail -1
sleep 12
snap deeplink
for _ in 1 2 3; do
  tap "Add" "OK" "Import" "Yes" "Confirm" "Добавить" "Импорт" "Да" "Подтвердить" || break
  sleep 8
done
snap added
sleep 20

# Запасные пути, если приложение не обработало ссылку: другая схема, затем ввод адреса в интерфейсе.
if ! captured && [ -n "${LINK2:-}" ]; then
  adb shell am start -a android.intent.action.VIEW -d "$LINK2" 2>&1 | tail -1
  sleep 15
  snap deeplink2
  tap "Add" "OK" "Import" "Yes" "Confirm" || true
  sleep 15
fi
if ! captured && [ -n "${SUB_URL:-}" ]; then
  tap "Add" && sleep 4
  snap add-dialog
  adb shell input text "$SUB_URL"
  sleep 2
  snap typed
  adb shell input keyevent KEYCODE_BACK
  tap "Add" "OK" "Import" "Save" "Confirm" || true
  sleep 15
  snap ui-added
fi
snap final
[ -n "$pkg" ] && adb logcat -d --pid="$(adb shell pidof "$pkg" | awk '{print $1}')" >"$out/logcat-app.txt" 2>&1
adb logcat -d -t 2000 >"$out/logcat.txt" 2>&1 || true
ls -la "$out"
