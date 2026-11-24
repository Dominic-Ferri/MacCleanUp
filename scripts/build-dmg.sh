#!/bin/zsh
# Packs build/<version>/MacCleanUp.app into a compressed, self-contained
# disk image with an Applications shortcut, then checks the image.
# Run ./scripts/build-app.sh first.
set -euo pipefail

ROOT="${0:A:h:h}"
VERSION="$(awk -F'"' '/^version = / {print $2; exit}' "$ROOT/Cargo.toml")"
APP="$ROOT/build/$VERSION/MacCleanUp.app"
DMG="$ROOT/build/$VERSION/MacCleanUp-$VERSION.dmg"
STAGE="$HOME/.local/share/maccleanup/dmg-$(date +%Y%m%d-%H%M%S)"
WORK="$STAGE.rw.dmg"
VOLUME="MacCleanUp $VERSION"

if [[ ! -d "$APP" ]]; then
    echo "error: missing $APP; run ./scripts/build-app.sh first" >&2
    exit 1
fi
if [[ -e "$DMG" ]]; then
    echo "error: $DMG already exists; move it aside before rebuilding this version" >&2
    exit 1
fi

mkdir -p "$STAGE"
ditto "$APP" "$STAGE/MacCleanUp.app"
ln -s /Applications "$STAGE/Applications"
cp "$ROOT/assets/MacCleanUp.icns" "$STAGE/.VolumeIcon.icns"
cat > "$STAGE/Read Me First.txt" <<TEXT
MacCleanUp $VERSION for Mac computers with Apple silicon (M1 or later)

Install: drag MacCleanUp onto Applications.

Or run it without installing: double-click MacCleanUp right here, or copy
this disk image to a thumb drive and open it on any Apple silicon Mac.
Nothing else needs to be installed, and the app keeps no settings files.

The first time you open a copy you downloaded, macOS may say it cannot
verify the developer, because MacCleanUp is not notarized. Open System
Settings, then Privacy & Security, and click Open Anyway.

macOS may also ask for access to Desktop, Documents and Downloads when a
scan reads them, and whether MacCleanUp may control Finder when Finder
has to move an item to the Trash for it.

MacCleanUp moves items only to the Trash, and only after you review them.
TEXT

# Writable image first, so the volume can get its custom icon; -nobrowse
# keeps it out of Finder, so no window opens.
hdiutil create -quiet -volname "$VOLUME" -srcfolder "$STAGE" -fs HFS+ -format UDRW -ov "$WORK"
MOUNT="$(hdiutil attach -nobrowse -noverify -noautoopen "$WORK" | awk -F'\t' '/\/Volumes\// {print $NF}')"
SetFile -a C "$MOUNT"
hdiutil detach -quiet "$MOUNT"
hdiutil convert -quiet "$WORK" -format UDZO -imagekey zlib-level=9 -o "$DMG"

# Check the finished image the way a user opens it: read-only.
hdiutil verify -quiet "$DMG"
MOUNT="$(hdiutil attach -nobrowse -readonly -noautoopen "$DMG" | awk -F'\t' '/\/Volumes\// {print $NF}')"
codesign --verify --deep --strict "$MOUNT/MacCleanUp.app"
ARCHS="$(lipo -archs "$MOUNT/MacCleanUp.app/Contents/MacOS/MacCleanUp")"
REPORTED="$("$MOUNT/MacCleanUp.app/Contents/MacOS/MacCleanUp" --version)"
hdiutil detach -quiet "$MOUNT"
if [[ "$ARCHS" != "arm64" || "$REPORTED" != "MacCleanUp $VERSION" ]]; then
    echo "error: image holds '$ARCHS' code reporting '$REPORTED'" >&2
    exit 1
fi
echo "Built: $DMG"
shasum -a 256 "$DMG"
