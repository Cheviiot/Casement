#!/bin/sh
# Removes the private application directory and desktop integration. Settings, device
# identity and trusted computers stay unless --settings is explicitly given.
set -eu

root=${CASEMENT_INSTALL_ROOT:-$HOME/.local/opt/casement}
share=${XDG_DATA_HOME:-$HOME/.local/share}
config=${XDG_CONFIG_HOME:-$HOME/.config}
case "$root" in
    /*) ;;
    *) echo "Каталог удаления должен быть абсолютным: $root" >&2; exit 1 ;;
esac
case "$root" in
    ""|/|"$HOME"|"$HOME/.local"|"$HOME/.local/opt")
        echo "Небезопасный каталог удаления: $root" >&2
        exit 1
        ;;
esac

if command -v systemctl >/dev/null 2>&1 && [ "${CASEMENT_SKIP_SERVICE:-0}" != 1 ]; then
    systemctl --user disable --now casement-host.service 2>/dev/null || true
fi
rm -f "$share/applications/io.github.cheviiot.Casement.desktop" \
    "$share/icons/hicolor/scalable/apps/io.github.cheviiot.Casement.svg" \
    "$share/metainfo/io.github.cheviiot.Casement.metainfo.xml" \
    "$config/systemd/user/casement-host.service" \
    "$HOME/.local/bin/casement" "$HOME/.local/bin/casement-host" \
    "$HOME/.local/bin/casement-cli" "$share/casement-uninstall.sh"
rm -rf -- "$root"
if [ "${1:-}" = "--settings" ]; then rm -rf -- "$config/casement"; fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$share/applications" >/dev/null 2>&1 || true
fi
if command -v systemctl >/dev/null 2>&1 && [ "${CASEMENT_SKIP_SERVICE:-0}" != 1 ]; then
    systemctl --user daemon-reload 2>/dev/null || true
fi
echo "Casement удалён."
