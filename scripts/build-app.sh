#!/bin/zsh
set -euo pipefail

ROOT="${0:A:h:h}"
VERSION="$(awk -F'"' '/^version = / {print $2; exit}' "$ROOT/Cargo.toml")"
# Fresh staging folder per build, so files from an older bundle layout
# (such as the 0.2 Terminal launcher) never end up sealed into this one.
STAGE="$HOME/.local/share/maccleanup/stage-$(date +%Y%m%d-%H%M%S)"
APP="$STAGE/MacCleanUp.app"
FINAL_DIR="$ROOT/build/$VERSION"
FINAL="$FINAL_DIR/MacCleanUp.app"
ARM="$ROOT/target/aarch64-apple-darwin/release/MacCleanUp"
INTEL="$ROOT/target/x86_64-apple-darwin/release/MacCleanUp"
ICON="$ROOT/assets/MacCleanUp.icns"

if [[ ! -f "$ICON" ]]; then
    echo "error: missing $ICON" >&2
    exit 1
fi
if [[ -e "$FINAL" ]]; then
    echo "error: $FINAL already exists; move it aside before rebuilding this version" >&2
    exit 1
fi

export MACOSX_DEPLOYMENT_TARGET=12.0
cargo build --manifest-path "$ROOT/Cargo.toml" --release --target aarch64-apple-darwin
cargo build --manifest-path "$ROOT/Cargo.toml" --release --target x86_64-apple-darwin

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/LICENSE" "$APP/Contents/Resources/LICENSE"
cp "$ICON" "$APP/Contents/Resources/MacCleanUp.icns"
lipo -create "$ARM" "$INTEL" -output "$APP/Contents/MacOS/MacCleanUp"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDisplayName</key><string>MacCleanUp</string>
    <key>CFBundleIconFile</key><string>MacCleanUp</string>
    <key>CFBundleIconName</key><string>MacCleanUp</string>
    <key>CFBundleExecutable</key><string>MacCleanUp</string>
    <key>CFBundleIdentifier</key><string>com.dferri.maccleanup</string>
    <key>CFBundleName</key><string>MacCleanUp</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>6</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
    <key>NSDesktopFolderUsageDescription</key><string>MacCleanUp looks for leftover installer files and measures folder sizes on your Desktop. It moves nothing without your review.</string>
    <key>NSDocumentsFolderUsageDescription</key><string>MacCleanUp looks for leftover installer files and measures folder sizes in Documents. It moves nothing without your review.</string>
    <key>NSDownloadsFolderUsageDescription</key><string>MacCleanUp looks for leftover installer files and measures folder sizes in Downloads. It moves nothing without your review.</string>
    <key>NSAppleEventsUsageDescription</key><string>When macOS does not let MacCleanUp move an item you reviewed, MacCleanUp asks Finder to move it to the Trash instead. Put Back still works.</string>
</dict>
</plist>
PLIST

chmod 755 "$APP/Contents/MacOS/MacCleanUp"
xattr -cr "$APP"
signed=0
for attempt in 1 2 3; do
    if codesign --force --sign - "$APP" 2>/dev/null; then
        signed=1
        break
    fi
    xattr -cr "$APP"
    /bin/sleep 1
done
if [[ $signed -ne 1 ]]; then
    echo "error: ad-hoc codesign failed" >&2
    exit 1
fi
codesign --verify --deep --strict "$APP"
mkdir -p "$FINAL_DIR"
ditto "$APP" "$FINAL"
xattr -d com.apple.FinderInfo "$FINAL" 2>/dev/null || true
xattr -d 'com.apple.fileprovider.fpfs#P' "$FINAL" 2>/dev/null || true
codesign --verify --strict "$FINAL/Contents/MacOS/MacCleanUp"
plutil -lint "$FINAL/Contents/Info.plist"
lipo -info "$FINAL/Contents/MacOS/MacCleanUp"
echo "Built: $FINAL"
