#!/bin/sh
# Packs the macOS build into "Simple Player.app" and a .dmg.
#
#   sh tools/macos/bundle.sh <SimplePlayer binary> <ffmpeg binary or "-"> <output dir>
#
# Run on a Mac (needs iconutil, codesign, hdiutil). The GitHub Actions workflow
# (.github/workflows/build.yml) calls it with a universal binary made by lipo.
set -e
BIN=$1
FFMPEG=$2
OUT=$(mkdir -p "$3" && cd "$3" && pwd)
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)

APP="$OUT/Simple Player.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/SimplePlayer"
chmod 755 "$APP/Contents/MacOS/SimplePlayer"
if [ "$FFMPEG" != "-" ] && [ -f "$FFMPEG" ]; then
  # src/ffdec.rs looks for ffmpeg next to the executable
  cp "$FFMPEG" "$APP/Contents/MacOS/ffmpeg"
  chmod 755 "$APP/Contents/MacOS/ffmpeg"
  cp "$ROOT/tools/ffmpeg-lite/FFmpeg-LICENSE.txt" "$APP/Contents/Resources/"
fi

sed "s/__VERSION__/$VERSION/g" "$ROOT/tools/macos/Info.plist" > "$APP/Contents/Info.plist"

# icon: the PNG layers inside assets/app.ico → AppIcon.icns
ICONSET="$OUT/AppIcon.iconset"
rm -rf "$ICONSET"
python3 "$ROOT/tools/macos/ico2iconset.py" "$ROOT/assets/app.ico" "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

# Apple Silicon refuses unsigned code: sign ad hoc (no Apple ID needed). Users still have
# to right-click → Open the first time, since it isn't notarized.
codesign --force --deep --sign - "$APP"

DMG="$OUT/SimplePlayer-$VERSION-macOS.dmg"
rm -f "$DMG"
STAGE="$OUT/dmg"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Simple Player" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"
echo "built: $DMG"
