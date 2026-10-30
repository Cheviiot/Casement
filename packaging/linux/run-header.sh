#!/bin/sh
# Self-extracting Casement installer. The package builder replaces the two values below
# and appends a gzip-compressed tar archive after this script.
set -eu

payload_line=@PAYLOAD_LINE@
payload_sha256=@PAYLOAD_SHA256@
install_root="${CASEMENT_INSTALL_ROOT:-$HOME/.local/opt/casement}"

usage() {
    cat <<EOF
Casement installer

Usage: $0 [--check | --extract DIRECTORY | --uninstall | --help]

Without an option, installs Casement for the current user into:
  $install_root
EOF
}

checksum() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 | sed 's/^.*= //'
    else
        echo "Для проверки установщика нужен sha256sum, shasum или openssl." >&2
        exit 1
    fi
}

verify() {
    actual=$(tail -n +"$payload_line" "$0" | checksum)
    if [ "$actual" != "$payload_sha256" ]; then
        echo "Установщик Casement повреждён: неверная контрольная сумма." >&2
        exit 1
    fi
}

case "${1:-}" in
    --help|-h) usage; exit 0 ;;
    --check)
        verify
        echo "Установщик Casement исправен."
        exit 0
        ;;
    --uninstall)
        if [ ! -x "$install_root/uninstall.sh" ]; then
            echo "Casement не установлен в $install_root" >&2
            exit 1
        fi
        shift
        exec "$install_root/uninstall.sh" "$@"
        ;;
    --extract)
        [ "$#" -eq 2 ] || { usage >&2; exit 2; }
        verify
        mkdir -p "$2"
        tail -n +"$payload_line" "$0" | gzip -dc | tar -xf - -C "$2"
        echo "Casement распакован в $2"
        exit 0
        ;;
    "") ;;
    *) usage >&2; exit 2 ;;
esac

verify
base=${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}
work=$(mktemp -d "$base/casement-installer.XXXXXX" 2>/dev/null || \
    mktemp -d "${TMPDIR:-/tmp}/casement-installer.XXXXXX")
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT HUP INT TERM
tail -n +"$payload_line" "$0" | gzip -dc | tar -xf - -C "$work"
package=$(find "$work" -mindepth 1 -maxdepth 1 -type d -name 'casement-*-linux-x86_64' -print -quit)
[ -n "$package" ] || { echo "В установщике нет пакета Casement." >&2; exit 1; }
sh "$package/install.sh"
exit $?
