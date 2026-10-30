#!/bin/sh
# Runs as root in a Debian 13 container from the repository root (locally through
# scripts/package-deb, in CI as the job's container): makes the .deb of the release
# binaries in BIN_DIR (default target/release), with Depends computed by dpkg-shlibdeps
# against Debian's own libraries, and copies it to dist/.
set -e
version=$(sh scripts/version)
bin=${BIN_DIR:-target/release}
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq --no-install-recommends dpkg-dev libgtk-4-1 libadwaita-1-0 \
    libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 libgstreamer-gl1.0-0 >/dev/null

root=$(mktemp -d)
pkg=$root/pkg
install -Dm755 "$bin/casement" "$pkg/usr/bin/casement"
install -Dm755 "$bin/casement-host" "$pkg/usr/bin/casement-host"
install -Dm755 "$bin/casement-cli" "$pkg/usr/bin/casement-cli"
install -Dm644 data/io.github.cheviiot.Casement.desktop \
    "$pkg/usr/share/applications/io.github.cheviiot.Casement.desktop"
install -Dm644 data/io.github.cheviiot.Casement.svg \
    "$pkg/usr/share/icons/hicolor/scalable/apps/io.github.cheviiot.Casement.svg"
install -Dm644 -t "$pkg/usr/share/icons/hicolor/scalable/apps" \
    data/icons/hicolor/scalable/apps/*.svg
mkdir -p "$pkg/usr/lib/systemd/user"
sed 's|@BINDIR@|/usr/bin|' data/casement-host.service > "$pkg/usr/lib/systemd/user/casement-host.service"
install -Dm644 data/io.github.cheviiot.Casement.metainfo.xml "$pkg/usr/share/metainfo/io.github.cheviiot.Casement.metainfo.xml"
install -Dm644 LICENSE "$pkg/usr/share/doc/casement/copyright"
install -Dm644 data/icons/LICENSE.devicon "$pkg/usr/share/doc/casement/LICENSE.devicon"

# dpkg-shlibdeps wants a debian/control next to it.
mkdir -p "$root/debian"
printf 'Source: casement\n\nPackage: casement\nArchitecture: amd64\n' > "$root/debian/control"
depends=$(cd "$root" && dpkg-shlibdeps -O "pkg/usr/bin/casement" "pkg/usr/bin/casement-host" \
    "pkg/usr/bin/casement-cli" | sed -n 's/^shlibs:Depends=//p')

mkdir -p "$pkg/DEBIAN"
# Accepting connections is on by default, for every user from their next login.
cat > "$pkg/DEBIAN/postinst" <<'SCRIPT'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    systemctl --global enable casement-host.service >/dev/null 2>&1 || true
fi
SCRIPT
cat > "$pkg/DEBIAN/prerm" <<'SCRIPT'
#!/bin/sh
set -e
if [ "$1" = remove ]; then
    systemctl --global disable casement-host.service >/dev/null 2>&1 || true
fi
SCRIPT
chmod 755 "$pkg/DEBIAN/postinst" "$pkg/DEBIAN/prerm"
cat > "$pkg/DEBIAN/control" <<CONTROL
Package: casement
Version: $version
Architecture: amd64
Maintainer: Cheviiot <6terio9@vk.com>
Depends: $depends, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-pipewire
Recommends: gstreamer1.0-libav
Section: net
Priority: optional
Homepage: https://github.com/Cheviiot/Casement
Description: Remote desktop for the local network and Tailscale
 Casement shows and controls another computer over the local network or
 Tailscale: screen in H.264, keyboard and mouse, clipboard text, sound and
 files both ways. The same application accepts connections to this computer
 (GNOME on Wayland) through the user service casement-host.
CONTROL
mkdir -p dist
dpkg-deb --root-owner-group --build "$pkg" "dist/casement_${version}_amd64.deb" >/dev/null
rm -rf "$root"
ls dist/*.deb
