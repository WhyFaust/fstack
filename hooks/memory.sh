#!/bin/sh
set -eu
[ -n "${PLUGIN_ROOT:-}" ] || exit 0
binary=$PLUGIN_ROOT/bin/fstack
[ -f "$binary" ] && [ ! -L "$binary" ] && [ -x "$binary" ] || exit 0
exec "$binary" --package "$PLUGIN_ROOT" memory hook "$1"
