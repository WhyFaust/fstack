#!/bin/sh
set -eu
[ -n "${PLUGIN_ROOT:-}" ] || exit 0
launcher=$PLUGIN_ROOT/launchers/fstack.sh
if [ "$1" = SessionStart ]; then
    operation=--install-runtime
else
    operation=--runtime-path
fi
if ! binary=$(/bin/sh "$launcher" "$operation"); then
    if [ "$1" != SubagentStart ]; then
        command -p cat "$PLUGIN_ROOT/hooks/runtime-unavailable.json"
    fi
    exit 0
fi
exec "$binary" --package "$PLUGIN_ROOT" hook "$1"
