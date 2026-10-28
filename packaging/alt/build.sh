#!/bin/sh
# Runs as root inside registry.altlinux.org/p11/base from the repository root (locally
# through scripts/package-alt, in CI as the job's container): installs rpm-build, builds
# the RPM of the release binaries in BIN_DIR (default target/release) as an ordinary user
# (ALT's rpmbuild refuses root) and copies it to dist/. REL is the RPM release (alt1).
set -e
version=$(sh scripts/version)
bin=${BIN_DIR:-target/release}
apt-get update -qq >/dev/null
# The libraries the binaries link to: ALT's dependency finder resolves them on this system.
apt-get install -y -qq rpm-build rpm-macros-systemd \
    libgtk4 libadwaita libgraphene libgio libgstreamer1.0 libgst-plugins1.0 >/dev/null
id builder >/dev/null 2>&1 || useradd -m builder
top=$(runuser -u builder -- mktemp -d)
mkdir -p "$top/SOURCES"
cp "$bin/casement" "$bin/casement-host" "$bin/casement-cli" \
    data/io.github.cheviiot.Casement.desktop data/io.github.cheviiot.Casement.svg \
    data/casement-host.service data/io.github.cheviiot.Casement.metainfo.xml packaging/alt/casement.spec "$top/SOURCES/"
chown -R builder "$top"
runuser -u builder -- rpmbuild -bb "$top/SOURCES/casement.spec" \
    --define "_topdir $top" --define "_sourcedir $top/SOURCES" \
    --define "ver $version" --define "rel ${REL:-alt1}" \
    --define "date_changelog $(LC_ALL=C date '+%a %b %d %Y')"
mkdir -p dist
cp "$top"/RPMS/*/*.rpm dist/
rm -rf "$top"
ls dist/*.rpm
