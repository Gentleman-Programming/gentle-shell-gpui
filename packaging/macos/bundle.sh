#!/usr/bin/env bash
# Builds dist/Gentle Shell.app, dist/gentle-shell-gpui-<version>-macos-<arch>.zip
# (what the in-app updater downloads) and a .dmg of the same signed app with an
# Applications shortcut (what people download to install by dragging).
#
#   packaging/macos/bundle.sh <target-triple> <version>
#   packaging/macos/bundle.sh aarch64-apple-darwin 0.1.0
#
# Set GENTLE_BIN=<path> to bundle an already-built binary instead of running
# `cargo build --profile dist --target <target-triple>` (the shipping profile);
# the terminal host is then GENTLE_TERM_HOST_BIN, else `gentle-term-host`
# beside it, and left out when neither is there. The bundled
# `gentle-term-host` is what the app offers to install on a Gentle Shell
# server of the same platform and architecture.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <target-triple> <version>" >&2
  exit 2
fi
target="$1"
version="$2"
case "$target" in
  aarch64-apple-darwin) arch=aarch64 ;;
  x86_64-apple-darwin) arch=x86_64 ;;
  *) echo "unsupported macOS target: $target" >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
min_macos=11.0

if [[ -n "${GENTLE_BIN:-}" ]]; then
  binary="$GENTLE_BIN"
  term_host="${GENTLE_TERM_HOST_BIN:-$(dirname "$binary")/gentle-term-host}"
else
  MACOSX_DEPLOYMENT_TARGET="$min_macos" \
    cargo build --profile dist --locked -p gentle-shell-gpui -p gentle-term-host --target "$target"
  binary="target/$target/dist/gentle-shell-gpui"
  term_host="target/$target/dist/gentle-term-host"
fi
[[ -x "$binary" ]] || { echo "missing binary: $binary" >&2; exit 1; }

app="dist/Gentle Shell.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/gentle-shell-gpui"
if [[ -x "$term_host" ]]; then
  cp "$term_host" "$app/Contents/MacOS/gentle-term-host"
fi
cp packaging/icons/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
sed -e "s/@VERSION@/$version/g" -e "s/@MIN_MACOS@/$min_macos/g" \
  packaging/macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Stray xattrs (quarantine, Finder info) would break the signature seal.
xattr -cr "$app"
codesign --force --deep --sign - "$app"

zip="dist/gentle-shell-gpui-$version-macos-$arch.zip"
rm -f "$zip"
# No AppleDouble `._*` entries: non-Apple unzippers (incl. the in-app
# updater) would write them into the bundle and invalidate the seal.
ditto -c -k --norsrc --noextattr --noacl --keepParent "$app" "$zip"

dmg="dist/gentle-shell-gpui-$version-macos-$arch.dmg"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
ditto --norsrc --noextattr --noacl "$app" "$stage/Gentle Shell.app"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname "Gentle Shell" -srcfolder "$stage" -ov -format UDZO "$dmg"

echo "$app"
echo "$zip"
echo "$dmg"
