#!/bin/sh

workspace=$(pwd -P) || exit 0
[ "$workspace" != / ] || exit 0
profile=${CODEX_HOME:-${HOME:+$HOME/.codex}}
[ -n "$profile" ] || exit 0

trusted_file() {
    case "$1" in
        /*) ;;
        *) return 1 ;;
    esac
    case "$1/" in
        "$workspace/"*|*/./*|*/../*|*//*) return 1 ;;
    esac
    [ -f "$1" ] && [ ! -L "$1" ] || return 1
    directory=${1%/*}
    while [ -n "$directory" ]; do
        [ ! -L "$directory" ] &&
            [ ! -e "$directory/.git" ] &&
            [ ! -L "$directory/.git" ] || return 1
        directory=${directory%/*}
    done
}

pin=$profile/fstack/memory/python-executable
trusted_file "$pin" || exit 0
IFS= read -r python < "$pin" || exit 0
trusted_file "$python" && [ -x "$python" ] || exit 0
exec "$python" -E -s -B "$PLUGIN_ROOT/skills/use-memory/scripts/memory.py" hook "$1"
