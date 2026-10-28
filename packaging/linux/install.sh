#!/bin/sh
# Installs Casement for the current user into ~/.local: the programs, the menu entry and
# the systemd user service casement-host, switched on: accepting connections is on by
# default ("Принимать подключения" switches it). Run it from an unpacked release archive,
# or straight from the web, which fetches the latest release first:
#   curl -fsSL https://github.com/Cheviiot/Casement/releases/latest/download/install.sh | sh
set -eu
repo=Cheviiot/Casement
here=$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)

if [ -z "$here" ] || [ ! -x "$here/bin/casement" ]; then
    # Run from the web: fetch and unpack the latest release, then install from it.
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    url="https://github.com/$repo/releases/latest/download/casement-linux-x86_64.tar.gz"
    echo "Скачиваю $url"
    curl -fsSL "$url" | tar -xz -C "$work"
    sh "$work"/casement-*/install.sh
    exit 0
fi

bin="$HOME/.local/bin"
share="${XDG_DATA_HOME:-$HOME/.local/share}"
units="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$bin" "$share/applications" "$share/icons/hicolor/scalable/apps" "$share/metainfo" "$units"
for b in casement casement-host casement-cli; do
    install -m 755 "$here/bin/$b" "$bin/$b.new"
    mv "$bin/$b.new" "$bin/$b"
done
sed "s|^Exec=casement|Exec=$bin/casement|" \
    "$here/share/applications/io.github.cheviiot.Casement.desktop" \
    > "$share/applications/io.github.cheviiot.Casement.desktop"
install -m 644 "$here/share/icons/hicolor/scalable/apps/io.github.cheviiot.Casement.svg" \
    "$share/icons/hicolor/scalable/apps/"
sed "s|@BINDIR@|$bin|" "$here/share/casement/casement-host.service" \
    > "$units/casement-host.service"
install -m 644 "$here/share/metainfo/io.github.cheviiot.Casement.metainfo.xml" "$share/metainfo/"
install -m 755 "$here/uninstall.sh" "$share/casement-uninstall.sh"
if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload 2>/dev/null || true
    # Accepting connections is on by default; "casement-cli accept off" turns it off.
    systemctl --user enable casement-host.service 2>/dev/null || true
    systemctl --user restart casement-host.service 2>/dev/null || true
fi

missing=$(ldd "$bin/casement" 2>/dev/null | sed -n 's/^\s*\(\S*\) => not found/\1/p' | tr '\n' ' ')
if [ -n "$missing" ]; then
    echo "Нет библиотек: $missing" >&2
    echo "Нужны GTK 4.14+, Libadwaita 1.6+ и GStreamer 1.24+ из пакетов вашего дистрибутива." >&2
fi
echo "Casement установлен в $bin. Удаление: sh $share/casement-uninstall.sh"
