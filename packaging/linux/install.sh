#!/bin/sh
# Installs the self-contained Casement runtime into its own per-user directory. Only the
# desktop entry, icon, metadata and systemd user unit live in their standard XDG places.
set -eu

repo=Cheviiot/Casement
here=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd -P || true)

checksum_file() {
    expected=$1
    file=$2
    if command -v sha256sum >/dev/null 2>&1; then
        printf '%s  %s\n' "$expected" "$file" | sha256sum -c - >/dev/null
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
        [ "$actual" = "$expected" ]
    elif command -v openssl >/dev/null 2>&1; then
        actual=$(openssl dgst -sha256 "$file" | sed 's/^.*= //')
        [ "$actual" = "$expected" ]
    else
        echo "Для проверки загрузки нужен sha256sum, shasum или openssl." >&2
        return 1
    fi
}

if [ -z "$here" ] || [ ! -x "$here/app/bin/casement" ]; then
    command -v curl >/dev/null 2>&1 || { echo "Для загрузки нужен curl." >&2; exit 1; }
    base=${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}
    work=$(mktemp -d "$base/casement-download.XXXXXX" 2>/dev/null || \
        mktemp -d "${TMPDIR:-/tmp}/casement-download.XXXXXX")
    trap 'rm -rf -- "$work"' EXIT HUP INT TERM
    release="https://github.com/$repo/releases/latest/download"
    echo "Скачиваю Casement…"
    curl -fsSL "$release/casement-linux-x86_64.run" -o "$work/casement.run"
    curl -fsSL "$release/SHA256SUMS" -o "$work/SHA256SUMS"
    expected=$(awk '$2 == "casement-linux-x86_64.run" || $2 == "*casement-linux-x86_64.run" {print $1; exit}' \
        "$work/SHA256SUMS")
    [ -n "$expected" ] || { echo "В релизе нет контрольной суммы установщика." >&2; exit 1; }
    checksum_file "$expected" "$work/casement.run" || {
        echo "Загруженный установщик Casement повреждён." >&2; exit 1; }
    sh "$work/casement.run"
    exit 0
fi

version=$(cat "$here/VERSION")
root=${CASEMENT_INSTALL_ROOT:-$HOME/.local/opt/casement}
case "$root" in
    /*) ;;
    *) echo "Каталог установки должен быть абсолютным: $root" >&2; exit 1 ;;
esac
case "$root" in
    ""|/|"$HOME"|"$HOME/.local"|"$HOME/.local/opt")
        echo "Небезопасный каталог установки: $root" >&2
        exit 1
        ;;
esac
share=${XDG_DATA_HOME:-$HOME/.local/share}
config=${XDG_CONFIG_HOME:-$HOME/.config}
units="$config/systemd/user"
desktop="$share/applications/io.github.cheviiot.Casement.desktop"
desktop_new="$desktop.new.$$"
desktop_old="$desktop.old.$$"
target="$root/versions/$version"
stage="$root/versions/.${version}.new.$$"
backup="$root/versions/.${version}.old.$$"
[ ! -e "$root/current" ] || [ -L "$root/current" ] || {
    echo "$root/current не является ссылкой; установка остановлена." >&2
    exit 1
}

if command -v systemctl >/dev/null 2>&1 && [ "${CASEMENT_SKIP_SERVICE:-0}" != 1 ]; then
    systemctl --user stop casement-host.service 2>/dev/null || true
fi
mkdir -p "$root/versions" "$share/applications" \
    "$share/icons/hicolor/scalable/apps" "$share/metainfo" "$units"
rm -rf -- "$stage" "$backup"
mkdir -p "$stage"
cp -a "$here/app/." "$stage/"

escape_sed() { printf '%s' "$1" | sed 's/[&|]/\\&/g'; }
escaped_root=$(escape_sed "$root")
find "$stage/share/glycin-loaders" -type f -name '*.conf' -exec \
    sed -i "s|@APPROOT@|$escaped_root|g" {} + 2>/dev/null || true

restore() {
    rm -rf -- "$stage"
    if [ -d "$backup" ] && [ ! -e "$target" ]; then mv "$backup" "$target"; fi
    rm -f -- "$desktop_new"
    if [ -f "$desktop_old" ]; then
        rm -f -- "$desktop"
        mv "$desktop_old" "$desktop"
    fi
}
trap restore EXIT HUP INT TERM
if [ -e "$target" ]; then mv "$target" "$backup"; fi
mv "$stage" "$target"
ln -sfn "versions/$version" "$root/.current.new.$$"
mv -Tf "$root/.current.new.$$" "$root/current"
install -m 755 "$here/uninstall.sh" "$root/uninstall.sh"

sed "s|^Exec=.*|Exec=\"$escaped_root/current/bin/casement\"|" \
    "$here/share/applications/io.github.cheviiot.Casement.desktop" > "$desktop_new"
chmod 644 "$desktop_new"
# GNOME Shell can keep an old Exec value when an existing desktop file is only
# truncated and rewritten. A remove/add transition plus desktop database refresh
# invalidates that cached GAppInfo while keeping the replacement recoverable.
if [ -f "$desktop" ] && ! cmp -s "$desktop_new" "$desktop"; then
    mv "$desktop" "$desktop_old"
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$share/applications" >/dev/null 2>&1 || true
    fi
fi
mv -f "$desktop_new" "$desktop"
rm -f -- "$desktop_old"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$share/applications" >/dev/null 2>&1 || true
fi
install -m 644 "$here/share/icons/hicolor/scalable/apps/io.github.cheviiot.Casement.svg" \
    "$share/icons/hicolor/scalable/apps/"
install -m 644 "$here/share/metainfo/io.github.cheviiot.Casement.metainfo.xml" "$share/metainfo/"
sed "s|^ExecStart=.*|ExecStart=\"$escaped_root/current/bin/casement-host\"|" \
    "$here/share/casement/casement-host.service" > "$units/casement-host.service"

# Remove files left by the former per-user layout. Distribution packages use /usr and
# are deliberately untouched.
rm -f "$HOME/.local/bin/casement" "$HOME/.local/bin/casement-host" \
    "$HOME/.local/bin/casement-cli" "$share/casement-uninstall.sh"
rm -rf -- "$backup"
trap - EXIT HUP INT TERM

if command -v systemctl >/dev/null 2>&1 && [ "${CASEMENT_SKIP_SERVICE:-0}" != 1 ]; then
    systemctl --user daemon-reload 2>/dev/null || true
    systemctl --user enable casement-host.service 2>/dev/null || true
    systemctl --user restart casement-host.service 2>/dev/null || true
fi
echo "Casement установлен в $root"
echo "Командная строка: $root/current/bin/casement-cli"
echo "Удаление: $root/uninstall.sh"
