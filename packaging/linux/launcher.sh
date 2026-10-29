#!/bin/sh
# Starts one of Casement's programs with the private runtime installed beside it.
set -eu

self_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
root=$(CDPATH= cd -- "$self_dir/.." && pwd -P)
program=$(basename -- "$0")
case "$program" in
    casement|casement-host|casement-cli) ;;
    *) echo "Unknown Casement program: $program" >&2; exit 2 ;;
esac

real="$root/libexec/casement/$program"
if [ ! -x "$root/libexec/casement-runtime" ] || [ ! -x "$real" ]; then
    echo "Casement runtime is incomplete: $root" >&2
    exit 1
fi

export XDG_DATA_DIRS="$root/share"
export GST_PLUGIN_PATH_1_0="$root/lib/gstreamer-1.0${GST_PLUGIN_PATH_1_0:+:$GST_PLUGIN_PATH_1_0}"
export GST_PLUGIN_SCANNER_1_0="$root/libexec/gstreamer-1.0/gst-plugin-scanner"
export GIO_EXTRA_MODULES="$root/lib/gio/modules${GIO_EXTRA_MODULES:+:$GIO_EXTRA_MODULES}"
export GLYCIN_DATA_DIR="$root/share/glycin-loaders"

cache="${XDG_CACHE_HOME:-$HOME/.cache}/casement"
mkdir -p "$cache"
export GST_REGISTRY_1_0="$cache/gstreamer-registry.bin"

CASEMENT_RUNTIME_ROOT=$root exec "$root/libexec/casement-runtime" "$real" "$@"
