#!/bin/sh
# Removes what install.sh put into ~/.local. Settings (~/.config/casement: the device key,
# the access password, trusted devices, saved computers) stay unless --settings is given.
set -eu
bin="$HOME/.local/bin"
share="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
if command -v systemctl >/dev/null 2>&1; then
    systemctl --user disable --now casement-host.service 2>/dev/null || true
fi
rm -f "$bin/casement" "$bin/casement-host" "$bin/casement-cli" \
    "$share/applications/io.github.cheviiot.Casement.desktop" \
    "$share/icons/hicolor/scalable/apps/io.github.cheviiot.Casement.svg" \
    "$share/metainfo/io.github.cheviiot.Casement.metainfo.xml" \
    "$config/systemd/user/casement-host.service" "$share/casement-uninstall.sh"
if [ "${1:-}" = "--settings" ]; then
    rm -rf "$config/casement"
fi
command -v systemctl >/dev/null 2>&1 && systemctl --user daemon-reload 2>/dev/null || true
echo "Casement удалён."
