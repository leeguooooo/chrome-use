#!/bin/sh
# Runs on the build box: fake panel (demo) + gateway in a temp HOME, then the
# UI walkthrough on the simulator (screenshots into $S/shots), then checks
# that the app registered for push and handed the gateway its relay token.
#
# Usage: simulator-walkthrough.sh [zh-Hans|en]
#   S        scratch dir holding ios/ (this folder), dd/ (DerivedData from
#            build-for-testing) and chrome-use (CLI built from this branch)
#   DEV      simulator UDID
#   SHOTS    where screenshots go (default $S/shots)
#   LIVE_PUSH=1  use the real push Worker (APNs sandbox to the simulator)
#            instead of the fixture's stand-in, and capture the delivered
#            notification after `mobile push-test`
set -u
S=${S:-$HOME/cu-ios-scratch}
DEV=${DEV:-BF28CD2A-E180-4812-A931-48984D59BF04}
export DEVELOPER_DIR=${DEVELOPER_DIR:-/Applications/Xcode-27.0.app/Contents/Developer}
LANG_=${1:-zh-Hans}
T=$(mktemp -d /tmp/cu-shots.XXXX); chmod 700 $T; mkdir -m 700 $T/home
export CHROME_USE_RELAY_DIR=$T/relay CHROME_USE_NO_UPDATE_CHECK=1
FIFO=$T/ctl; mkfifo $FIFO
(sleep 100000 > $FIFO) & KEEP=$!
HOME=$T/home python3 $S/ios/ChromeUseKit/Tests/ChromeUseKitTests/Fixtures/fake_panel.py --relay $T/relay --binary $S/chrome-use --demo --demo-lang $LANG_ < $FIFO > $T/panel.out 2>$T/panel.err &
PANEL=$!
sleep 1
PUSH=$(python3 -c "import json;print(json.loads(open('$T/panel.out').readline())['push'])")
[ "${LIVE_PUSH:-0}" = 1 ] && PUSH=https://chrome-use-push.leeguoo.com
HOME=$T/home $S/chrome-use mobile serve --bind 127.0.0.1:0 --push-url $PUSH > $T/gw.out 2>$T/gw.err &
GW=$!
sleep 2
ADDR=$(python3 -c "import json;print(json.loads(open('$T/gw.out').readline())['listening'])")
LINK=$(HOME=$T/home $S/chrome-use mobile pair --url ws://$ADDR/ --json | python3 -c "import json,sys;print(json.load(sys.stdin)['pairUri'])")
DEVICE=$(python3 -c "import urllib.parse,sys;print(urllib.parse.parse_qs(urllib.parse.urlparse(sys.argv[1]).query)['id'][0])" "$LINK")
SHOTS=${SHOTS:-$S/shots}; mkdir -p $SHOTS
xcrun simctl boot $DEV 2>/dev/null
xcrun simctl status_bar $DEV override --time 9:41 --batteryState charged --batteryLevel 100 --cellularBars 4 --wifiBars 3 2>/dev/null
xcrun simctl uninstall $DEV com.leeguoo.chromeuse 2>/dev/null
xcrun simctl keychain $DEV reset 2>/dev/null
cd $S/ios
TEST_RUNNER_PAIR_LINK="$LINK" TEST_RUNNER_SHOTS_DIR=$SHOTS TEST_RUNNER_SHOTS_LANGUAGE=$LANG_ TEST_RUNNER_PUSH_WORKER=$PUSH xcodebuild test-without-building -project ChromeUse.xcodeproj -scheme ChromeUse -destination id=$DEV -derivedDataPath $S/dd -only-testing:ChromeUseUITests -resultBundlePath $S/shots-$LANG_-$(date +%s).xcresult > $S/shots-$LANG_.log 2>&1
echo "UITEST EXIT $?"
# The app registered with the stand-in Worker and gave the gateway its token?
[ "${LIVE_PUSH:-0}" = 1 ] || grep -c '"register"' $T/panel.out | sed 's/^/registrations: /'
HOME=$T/home $S/chrome-use mobile devices --json | python3 -c "import json,sys;d=json.load(sys.stdin)['devices'];print('device push:', [x.get('push') for x in d])"
# A real push from the gateway, delivered to the simulator with simctl.
HOME=$T/home $S/chrome-use mobile push-test $DEVICE --push-url $PUSH
sleep 1
if [ "${LIVE_PUSH:-0}" = 1 ]; then
  # The banner shows for a few seconds; keep frames and the caller picks one.
  for i in 1 2 3 4 5 6 7 8; do
    sleep 1
    xcrun simctl io $DEV screenshot $SHOTS/notification-$i.png >/dev/null 2>&1
  done
  echo "notification frames taken"
else
python3 - "$T/panel.out" <<'PY'
import json, sys
pushes = [json.loads(l)["push"] for l in open(sys.argv[1]) if '"push": {' in l]
print("pushes recorded:", len(pushes), "last relayToken starts:", pushes[-1]["relayToken"][:8] if pushes else None)
PY
fi
kill $GW $PANEL $KEEP 2>/dev/null
rm -rf $T
echo DONE
