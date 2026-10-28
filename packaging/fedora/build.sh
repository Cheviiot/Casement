#!/bin/sh
# Runs as root in a Fedora container from the repository root (locally through
# scripts/package-fedora, in CI as the job's container): builds the RPM of the release
# binaries in BIN_DIR (default target/release) and copies it to dist/. REL is the release.
set -e
version=$(sh scripts/version)
bin=${BIN_DIR:-target/release}
dnf -y -q install rpm-build systemd-rpm-macros >/dev/null
top=$(mktemp -d)
mkdir -p "$top/SOURCES"
cp "$bin/casement" "$bin/casement-host" "$bin/casement-cli" \
    data/io.github.cheviiot.Casement.desktop data/io.github.cheviiot.Casement.svg \
    data/casement-host.service data/io.github.cheviiot.Casement.metainfo.xml LICENSE "$top/SOURCES/"
rpmbuild -bb packaging/fedora/casement.spec \
    --define "_topdir $top" --define "_sourcedir $top/SOURCES" \
    --define "ver $version" --define "rel ${REL:-1}" \
    --define "date_changelog $(LC_ALL=C date '+%a %b %d %Y')"
mkdir -p dist
cp "$top"/RPMS/*/*.rpm dist/
rm -rf "$top"
ls dist/*.fc*.rpm
