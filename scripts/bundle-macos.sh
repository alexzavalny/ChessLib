#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo build --release --locked
APP="dist/ChessLib.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/chesslib "$APP/Contents/MacOS/chesslib"
cp macos/Info.plist "$APP/Contents/Info.plist"
sdk_version=$(xcrun --sdk macosx --show-sdk-version 2>/dev/null || printf 0)
if [ "${sdk_version%%.*}" -ge 26 ] && xcrun --find actool >/dev/null 2>&1; then
    staging=$(mktemp -d)
    trap 'rm -rf "$staging"' EXIT
    mkdir -p "$staging/AppIcon.icon/Assets"
    cp macos/AppIcon.png "$staging/AppIcon.icon/Assets/"
    cp macos/icon.json "$staging/AppIcon.icon/"
    xcrun actool "$staging/AppIcon.icon" --compile "$APP/Contents/Resources" \
        --app-icon AppIcon --platform macosx --minimum-deployment-target 12.0 \
        --output-partial-info-plist "$staging/icon.plist"
    plutil -insert CFBundleIconName -string AppIcon "$APP/Contents/Info.plist"
fi
if command -v swift >/dev/null 2>&1; then
    swift scripts/icon.swift macos/AppIcon.png "$APP/Contents/Resources/AppIcon.icns"
fi
codesign --force --deep --sign - "$APP"
printf 'Built %s/%s\n' "$PWD" "$APP"
