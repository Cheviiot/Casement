#!/bin/sh
# Uses the host loader when it is new enough, so graphics and VA drivers remain a single
# coherent system stack. Older distributions fall back to the glibc shipped in the app.
set -eu

root=${CASEMENT_RUNTIME_ROOT:?}
real=$1
shift
required=$(cat "$root/lib/glibc/VERSION")
host=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}' || true)

version_ge() {
    left_major=${1%%.*}
    left_minor=${1#*.}; left_minor=${left_minor%%.*}
    right_major=${2%%.*}
    right_minor=${2#*.}; right_minor=${right_minor%%.*}
    case "$left_major:$left_minor:$right_major:$right_minor" in
        *[!0-9:]*|'') return 1 ;;
    esac
    [ "$left_major" -gt "$right_major" ] || {
        [ "$left_major" -eq "$right_major" ] && [ "$left_minor" -ge "$right_minor" ];
    }
}

if [ -n "$host" ] && version_ge "$host" "$required"; then
    export LD_LIBRARY_PATH="$root/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    exec "$real" "$@"
fi

loader="$root/lib/glibc/ld-linux-x86-64.so.2"
[ -x "$loader" ] || { echo "Casement runtime loader not found" >&2; exit 1; }
exec "$loader" --library-path "$root/lib/glibc:$root/lib/platform:$root/lib" "$real" "$@"
