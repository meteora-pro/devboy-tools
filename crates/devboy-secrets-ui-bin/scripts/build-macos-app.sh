#!/usr/bin/env bash
# K32 — Pack the release binary into a proper macOS `.app` bundle
# so the Dock shows the devboy brand icon instead of the generic
# "gear-with-cross" placeholder.
#
# Why a bundle:
#   * macOS sources the Dock-icon from the `.app`'s Info.plist
#     (`CFBundleIconFile`) — `setApplicationIconImage` at runtime
#     can't override that for a bare executable.
#   * `LSUIElement = false` (default) keeps the app in the Dock
#     as a regular foreground app, not an agent.
#
# Output: `target/release/DevboySecrets.app/` ready to launch
# via `open` or drag into `/Applications`.
#
# Re-run after every release rebuild — the script is idempotent
# (rm -rf + rebuild).

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../../.." && pwd)"
CRATE_DIR="$ROOT_DIR/crates/devboy-secrets-ui-bin"
TARGET_DIR="$ROOT_DIR/target/release"
APP_NAME="DevboySecrets"
APP_BUNDLE="$TARGET_DIR/$APP_NAME.app"
ICON_SRC="$CRATE_DIR/assets/devboy-logo.png"
BIN_SRC="$TARGET_DIR/devboy-secrets-ui"

if [ ! -f "$BIN_SRC" ]; then
  echo "release binary not found at $BIN_SRC — run \`cargo build --release -p devboy-secrets-ui-bin\` first" >&2
  exit 1
fi

if [ ! -f "$ICON_SRC" ]; then
  echo "icon PNG not found at $ICON_SRC" >&2
  exit 1
fi

# Bundle skeleton.
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

# Build the iconset → icns. macOS Dock needs an .icns file with
# multiple resolutions; `sips` resizes the source PNG into the
# standard set, `iconutil` packs them.
ICONSET_DIR="$(mktemp -d)/devboy-secrets.iconset"
mkdir -p "$ICONSET_DIR"
for size in 16 32 64 128 256 512; do
  sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET_DIR/icon_${size}x${size}.png" >/dev/null
  doubled=$((size * 2))
  if [ "$doubled" -le 1024 ]; then
    sips -z "$doubled" "$doubled" "$ICON_SRC" --out "$ICONSET_DIR/icon_${size}x${size}@2x.png" >/dev/null
  fi
done
iconutil -c icns "$ICONSET_DIR" -o "$APP_BUNDLE/Contents/Resources/app.icns"
rm -rf "$ICONSET_DIR"

# Copy the binary in.
cp "$BIN_SRC" "$APP_BUNDLE/Contents/MacOS/devboy-secrets-ui"
chmod +x "$APP_BUNDLE/Contents/MacOS/devboy-secrets-ui"

# Info.plist — the bare-minimum keys macOS needs to treat the
# bundle as a real app.
cat >"$APP_BUNDLE/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Devboy Secrets</string>
    <key>CFBundleDisplayName</key>
    <string>Devboy Secrets</string>
    <key>CFBundleIdentifier</key>
    <string>pro.meteora.devboy.secrets</string>
    <key>CFBundleVersion</key>
    <string>0.29.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.29.0</string>
    <key>CFBundleExecutable</key>
    <string>devboy-secrets-ui</string>
    <key>CFBundleIconFile</key>
    <string>app</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
EOF

# Refresh the Launch Services cache so the new icon shows up
# immediately (otherwise macOS may keep the old generic icon
# cached on the bundle path).
/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister \
  -f "$APP_BUNDLE" 2>/dev/null || true

echo "built $APP_BUNDLE"
echo "launch with: open '$APP_BUNDLE'"
echo "or:          '$APP_BUNDLE/Contents/MacOS/devboy-secrets-ui'"
