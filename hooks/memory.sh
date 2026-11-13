#!/bin/sh
set -eu
[ -n "${PLUGIN_ROOT:-}" ] || exit 0
if [ "$1" = Bootstrap ]; then
    binary=$(/bin/sh "$PLUGIN_ROOT/launchers/fstack.sh" --install-runtime)
    exec "$binary" --package "$PLUGIN_ROOT" migrate-profile --hook
fi
if ! binary=$(/bin/sh "$PLUGIN_ROOT/launchers/fstack.sh" --runtime-path 2>/dev/null); then
    if [ "$1" = UserPromptSubmit ]; then
        command -p cat "$PLUGIN_ROOT/hooks/runtime-unavailable.json"
    fi
    exit 0
fi
exec "$binary" --package "$PLUGIN_ROOT" memory hook "$1"
