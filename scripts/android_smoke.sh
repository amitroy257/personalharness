#!/usr/bin/env bash
# Install the debug APK on a running emulator, launch it, and run the app's
# own privacy self-check from inside Android. Used by the Android CI job.
#   scripts/android_smoke.sh path/to/app-debug.apk
set -euo pipefail
APK="$1"
PKG="io.github.amitroy257.personalharness"

echo "== install"
adb install -r "$APK"

launch() {
  adb shell monkey -p "$PKG" -c android.intent.category.LAUNCHER 1 >/dev/null
  for i in $(seq 1 60); do
    if adb shell pidof "$PKG" >/dev/null 2>&1; then echo "running after ${i}s (pid $(adb shell pidof "$PKG"))"; return 0; fi
    sleep 1
  done
  echo "app did not start"; adb logcat -d | tail -200; return 1
}

echo "== first launch"
launch
sleep 12
adb shell pidof "$PKG" >/dev/null || { echo "app exited after launch"; adb logcat -d | grep -iE "panic|fatal|$PKG" | tail -80; exit 1; }

echo "== app-private data"
SETTINGS=$(adb shell run-as "$PKG" find . -name settings.json 2>/dev/null | tr -d '\r' | head -1)
[ -n "$SETTINGS" ] || { echo "settings.json not created in private storage"; exit 1; }
echo "settings: /data/user/0/$PKG/${SETTINGS#./}"
adb shell run-as "$PKG" ls -la "$(dirname "$SETTINGS")"
grep -q '"enabled": false' <(adb shell run-as "$PKG" cat "$SETTINGS") && echo "local API is off by default on phones: OK"

echo "== enable the local API so the self-check can be queried, then relaunch"
adb shell run-as "$PKG" sed -i 's/"enabled": false/"enabled": true/' "$SETTINGS"
adb shell am force-stop "$PKG"
launch
adb forward tcp:4141 tcp:4141
for i in $(seq 1 60); do curl -sf http://127.0.0.1:4141/health && break; sleep 1; done
echo
TOKEN=$(adb shell run-as "$PKG" cat "$SETTINGS" | python3 -c "import json,sys; print(json.load(sys.stdin)['server']['token'])")
code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:4141/v1/models)
echo "unauthenticated /v1/models -> $code"; test "$code" = "401"
curl -sf -H "Authorization: Bearer $TOKEN" http://127.0.0.1:4141/v1/selfcheck > selfcheck.json
python3 scripts/ci_summary.py selfcheck selfcheck.json "Privacy self-check — Android app (emulator)" \
  --require os_sandbox,escape_other_apps,escape_read_app,dir_permissions,file_permissions,api_loopback_only,api_requires_token,api_host_check,api_no_cors

echo "== crash check"
if adb logcat -d | grep -E "FATAL EXCEPTION|panicked at" | grep -v "^--------- beginning"; then
  echo "crash found in logcat"; exit 1
fi
echo "Android smoke test passed"
