#!/bin/sh
# Runs a bundled helper (GStreamer scanner or Glycin loader) with Casement's loader.
set -eu

self_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
root=$self_dir
while [ ! -x "$root/libexec/casement-runtime" ]; do
    parent=$(dirname -- "$root")
    if [ "$parent" = "$root" ]; then
        echo "Casement runtime loader not found" >&2
        exit 1
    fi
    root=$parent
done

export XDG_DATA_DIRS="$root/share"
CASEMENT_RUNTIME_ROOT=$root exec "$root/libexec/casement-runtime" "$0.real" "$@"
