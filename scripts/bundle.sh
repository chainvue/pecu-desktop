#!/usr/bin/env bash
#
# Build Pecu.app.
#
#   scripts/bundle.sh                       # unsigned, for this machine only
#   CODESIGN_IDENTITY="Developer ID Application: Name (TEAMID)" scripts/bundle.sh
#
# Produces target/Pecu.app.
#
# ── What this does and does not do ───────────────────────────────────────────
#
# It assembles a bundle and, if you give it an identity, signs it. It does NOT
# notarise: that needs an App Store Connect key and a round trip to Apple, and
# the command is written out at the end of this file rather than run, because
# nobody should hand credentials to a script they have not read.
#
# Without notarisation the bundle runs on the machine that built it and is
# blocked by Gatekeeper everywhere else. That is the correct behaviour and not a
# bug in the bundle.
#
# ── Why a shell script and not cargo-bundle ──────────────────────────────────
#
# Because a bundle is a directory with three files in it, and the alternative is
# a build dependency that generates a plist this project would then have to
# override anyway. Everything here is a macOS built-in: iconutil, plutil,
# codesign.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

name="Pecu"
bundle_id="com.pecu.wallet"
app="target/${name}.app"
iconset_src="crates/pecu-app/assets/Pecu.iconset"

version="$(awk -F'"' '/^version/ { print $2; exit }' Cargo.toml)"
[ -n "$version" ] || { echo "could not read version from Cargo.toml" >&2; exit 1; }

echo "==> building ${name} ${version}"
cargo build --release -p pecu-app

[ -d "$iconset_src" ] || {
  echo "missing $iconset_src — run: cargo run -p pecu-ui --example render_icon" >&2
  exit 1
}

echo "==> icon"
# Straight from the checked-in iconset. Every size in it was **drawn** at that
# size by `render_icon`, not shrunk from one big one — a 1024px render reduced
# to 16 blurs the ring and the dot into a blob, which is what a Finder row and
# the ⌘-Tab strip would then show. Nothing here resamples anything.
mkdir -p "$(dirname "$app")"
rm -rf "$app"
mkdir -p "${app}/Contents/MacOS" "${app}/Contents/Resources"
iconutil -c icns "$iconset_src" -o "${app}/Contents/Resources/AppIcon.icns"

echo "==> bundle"
cp target/release/pecu "${app}/Contents/MacOS/${name}"

cat > "${app}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>              <string>${name}</string>
  <key>CFBundleDisplayName</key>       <string>${name}</string>
  <key>CFBundleIdentifier</key>        <string>${bundle_id}</string>
  <key>CFBundleVersion</key>           <string>${version}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleExecutable</key>        <string>${name}</string>
  <key>CFBundleIconFile</key>          <string>AppIcon</string>
  <key>CFBundlePackageType</key>       <string>APPL</string>
  <!-- The wallet draws its own title bar and needs the full window. -->
  <key>NSHighResolutionCapable</key>   <true/>
  <!-- No document types, no URL schemes, no services: this application is not
       a handler for anything, and declaring what it does not do is how an
       application ends up launched by something it never expected. -->
  <key>LSMinimumSystemVersion</key>    <string>11.0</string>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

plutil -lint "${app}/Contents/Info.plist" >/dev/null

if [ -n "${CODESIGN_IDENTITY:-}" ]; then
  echo "==> signing as ${CODESIGN_IDENTITY}"
  # --options runtime is what notarisation requires. --timestamp needs a
  # network round trip to Apple's timestamp authority.
  codesign --force --deep --options runtime --timestamp \
    --sign "$CODESIGN_IDENTITY" "$app"
  codesign --verify --strict --verbose=2 "$app"
else
  echo "==> not signed (set CODESIGN_IDENTITY to sign)"
fi

echo
echo "built ${app}"
du -sh "$app" | sed 's/^/       /'
echo
cat <<'NEXT'
To notarise — needs an App Store Connect API key, and cannot be done from here:

  ditto -c -k --keepParent target/Pecu.app /tmp/Pecu.zip
  xcrun notarytool submit /tmp/Pecu.zip \
      --key /path/to/AuthKey_XXXX.p8 --key-id XXXX --issuer <uuid> --wait
  xcrun stapler staple target/Pecu.app
  spctl -a -vvv -t install target/Pecu.app     # should say: accepted

Until that is done the bundle runs only on the machine that built it.
NEXT
