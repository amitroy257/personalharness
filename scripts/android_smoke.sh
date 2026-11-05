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

own_crash() {
  # a crash of *this* app: Java FATAL EXCEPTION in our process, or a Rust panic
  adb logcat -d | grep -E "FATAL EXCEPTION|panicked at|Process: $PKG" | grep -E "panicked at|Process: $PKG" >/dev/null
}

echo "== first launch"
launch
sleep 12
if ! adb shell pidof "$PKG" >/dev/null; then
  if own_crash; then
    echo "app crashed after launch"; adb logcat -d | grep -E -A20 "FATAL EXCEPTION|panicked at" | head -80; exit 1
  fi
  echo "app was stopped by the system without crashing (see below); relaunching once"
  adb logcat -d | grep -E "Killing .*$PKG" | tail -3
  launch
  sleep 12
  adb shell pidof "$PKG" >/dev/null || { echo "app exited again after relaunch"; adb logcat -d | grep -iE "panic|fatal|Killing .*$PKG" | tail -40; exit 1; }
fi

echo "== backup disabled (app data must not leave the phone via Android backup)"
if adb shell dumpsys package "$PKG" | grep -E "flags=|pkgFlags=" | grep -q "ALLOW_BACKUP"; then
  echo "ALLOW_BACKUP is set"; exit 1
fi
echo "ALLOW_BACKUP not set: OK"

echo "== app-private data"
SETTINGS=$(adb shell run-as "$PKG" find . -name settings.json 2>/dev/null | tr -d '\r' | head -1)
[ -n "$SETTINGS" ] || { echo "settings.json not created in private storage"; exit 1; }
echo "settings: /data/user/0/$PKG/${SETTINGS#./}"
adb shell run-as "$PKG" ls -la "$(dirname "$SETTINGS")"
grep -q '"enabled": false' <(adb shell run-as "$PKG" cat "$SETTINGS") && echo "local API is off by default on phones: OK"

echo "== enable the local API so the self-check can be queried, then relaunch"
# edit on the host and stream back (quotes do not survive adb's shell re-parsing)
adb shell am force-stop "$PKG"
adb shell run-as "$PKG" cat "$SETTINGS" | tr -d '\r' > settings.device.json
python3 -c "import json; d=json.load(open('settings.device.json')); d['server']['enabled']=True; json.dump(d, open('settings.device.json','w'), indent=2)"
adb push settings.device.json /data/local/tmp/ph-settings.json >/dev/null
adb shell chmod 644 /data/local/tmp/ph-settings.json
adb shell run-as "$PKG" cp /data/local/tmp/ph-settings.json "$SETTINGS"
adb shell rm -f /data/local/tmp/ph-settings.json
adb shell run-as "$PKG" cat "$SETTINGS" | grep -q '"enabled": true' || { echo "could not enable the local API"; exit 1; }
rm -f settings.device.json
launch
adb forward tcp:4141 tcp:4141
for i in $(seq 1 60); do curl -sf http://127.0.0.1:4141/health && break; sleep 1; done
echo
TOKEN=$(adb shell run-as "$PKG" cat "$SETTINGS" | python3 -c "import json,sys; print(json.load(sys.stdin)['server']['token'])")
code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:4141/v1/models)
echo "unauthenticated /v1/models -> $code"; test "$code" = "401"
curl -sf -H "Authorization: Bearer $TOKEN" http://127.0.0.1:4141/v1/selfcheck > selfcheck.json
python3 scripts/ci_summary.py selfcheck selfcheck.json "Privacy self-check — Android app (emulator)" \
  --require os_sandbox,escape_other_apps,escape_system_data,escape_read_app,dir_permissions,file_permissions,api_loopback_only,api_requires_token,api_host_check,api_no_cors

echo "== crash check"
if own_crash; then
  echo "crash of $PKG found in logcat"; adb logcat -d | grep -E -A20 "FATAL EXCEPTION|panicked at" | head -60; exit 1
fi
echo "Android smoke test passed"
