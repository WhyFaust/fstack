#!/bin/sh
set -eu
[ -n "${PLUGIN_ROOT:-}" ] || exit 0
if [ "$1" = Bootstrap ]; then
    /bin/sh "$PLUGIN_ROOT/launchers/fstack.sh" --install-runtime >/dev/null
    printf '%s\n' '{}'
    exit 0
fi
retries=0
if [ "$1" = SessionStart ]; then retries=6; fi
while ! binary=$(/bin/sh "$PLUGIN_ROOT/launchers/fstack.sh" --runtime-path 2>/dev/null); do
    if [ "$retries" -gt 0 ]; then
        command -p sleep 1
        retries=$((retries - 1))
        continue
    fi
    if [ "$1" = UserPromptSubmit ]; then
        command -p cat "$PLUGIN_ROOT/hooks/runtime-unavailable.json"
    fi
    exit 0
done
exec "$binary" --package "$PLUGIN_ROOT" memory hook "$1"
