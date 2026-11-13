#!/bin/sh
set -eu

package_root() {
    CDPATH= cd -P -- "$(command -p dirname -- "$0")/.." && pwd -P
}

runtime_path() {
    root=$1
    if [ -f "$root/bin/fstack" ] && [ ! -L "$root/bin/fstack" ] && [ -x "$root/bin/fstack" ]; then
        printf '%s\n' "$root/bin/fstack"
        return
    fi
    case "$(command -p uname -s):$(command -p uname -m)" in
        Linux:x86_64) target=x86_64-unknown-linux-gnu ;;
        Darwin:arm64) target=aarch64-apple-darwin ;;
        Darwin:x86_64)
            target=x86_64-apple-darwin
            if [ "$(/usr/sbin/sysctl -in sysctl.proc_translated 2>/dev/null || :)" = 1 ]; then
                target=aarch64-apple-darwin
            fi
            ;;
        *) printf '%s\n' 'fstack: неподдерживаемая ОС или архитектура' >&2; return 1 ;;
    esac
    version=$(command -p sed -nE 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"([0-9]+\.[0-9]+\.[0-9]+)"[,]?[[:space:]]*$/\1/p' "$root/.codex-plugin/plugin.json")
    if ! printf '%s\n' "$version" | command -p grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || [ "$(printf '%s\n' "$version" | command -p wc -l)" -ne 1 ]; then
        printf '%s\n' 'fstack: некорректная версия пакета' >&2
        return 1
    fi
    id_file=$root/launchers/runtime-id
    if ! IFS= read -r runtime_id < "$id_file" || [ "${#runtime_id}" -ne 64 ] ||
        [ "$(command -p wc -c < "$id_file")" -ne 65 ] ||
        ! printf '%s\n' "$runtime_id" | LC_ALL=C command -p grep -Eq '^[0-9a-f]{64}$'; then
        printf '%s\n' 'fstack: некорректный идентификатор runtime' >&2
        return 1
    fi
    runtime_home=${CODEX_HOME:-${HOME:?}/.codex}
    case "$runtime_home" in
        /*) ;;
        *) runtime_home=$PWD/$runtime_home ;;
    esac
    printf '%s\n' "$runtime_home/fstack/runtime/$version/$runtime_id/$target/fstack"
}

install_runtime() {
    binary=$1
    case "$binary" in "$2/bin/fstack") return ;; esac
    target_dir=${binary%/*}
    target=${target_dir##*/}
    source_dir=${target_dir%/*}
    runtime_id=${source_dir##*/}
    release_dir=${source_dir%/*}
    version=${release_dir##*/}
    asset=fstack-$version-$runtime_id-$target
    url=https://github.com/WhyFaust/fstack/releases/download/v$version/$asset

    umask 077
    mkdir -p -- "$target_dir"
    staging=$(mktemp -d "$target_dir/.download.XXXXXXXX")
    trap 'rm -rf -- "$staging"' EXIT
    trap 'exit 1' HUP INT TERM
    for suffix in .sha256 ''; do
        curl -q --proto '=https' --proto-redir '=https' --fail --silent --show-error --location \
            --connect-timeout 15 --max-time 180 --retry 2 --retry-max-time 180 \
            "$url$suffix" --output "$staging/$asset$suffix"
    done
    if command -v sha256sum >/dev/null 2>&1; then
        digest=$(sha256sum < "$staging/$asset")
    else
        digest=$(shasum -a 256 < "$staging/$asset")
    fi
    expected=$(cat "$staging/$asset.sha256")
    if [ "$expected" != "${digest%% *}  $asset" ] || [ ! -s "$staging/$asset" ]; then
        printf '%s\n' 'fstack: контрольная сумма runtime не совпала' >&2
        return 1
    fi
    chmod 700 "$staging/$asset"
    mv -f -- "$staging/$asset" "$binary"
}

main() {
    root=$(package_root)
    binary=$(runtime_path "$root")
    case "${1:-}" in
        --install-runtime)
            [ "$#" -eq 1 ] || exit 2
            if [ ! -f "$binary" ] || [ -L "$binary" ] || [ ! -x "$binary" ]; then
                install_runtime "$binary" "$root"
            fi
            printf '%s\n' "$binary"
            ;;
        --runtime-path)
            [ "$#" -eq 1 ] || exit 2
            [ -f "$binary" ] && [ ! -L "$binary" ] && [ -x "$binary" ] || exit 1
            printf '%s\n' "$binary"
            ;;
        *)
            if [ ! -f "$binary" ] || [ -L "$binary" ] || [ ! -x "$binary" ]; then
                printf '%s\n' 'fstack: откройте чат с одобренным хуком или запустите этот файл с --install-runtime' >&2
                exit 1
            fi
            exec "$binary" --package "$root" "$@"
            ;;
    esac
}

main "$@"
