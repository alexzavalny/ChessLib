#!/bin/sh
# Builds the release app and installs it as /Applications/ChessLib.app, replacing an older copy.
# The library is not touched: it lives in its own folder, not inside the app.
set -eu
cd "$(dirname "$0")/.."
DEST="/Applications/ChessLib.app"
# Replacing a running app leaves it executing a deleted binary.
if pgrep -f "$DEST/" >/dev/null; then
    echo "ChessLib is running from $DEST. Quit it (Cmd+Q) and run this again." >&2
    exit 1
fi
sh scripts/bundle-macos.sh
rm -rf "$DEST"
ditto dist/ChessLib.app "$DEST"
printf 'Installed %s\n' "$DEST"
