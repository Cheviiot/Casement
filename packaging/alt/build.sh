#!/bin/sh
# Runs as root in a Debian 13 container from the repository root (locally through
# scripts/package-alt, in CI as the job's container): builds the ALT p11 RPM of the release
# binaries in BIN_DIR (default target/release) and copies it to dist/. REL is the release.
# Debian's rpm 4.20 still writes the MD5/SHA1 digests ALT's rpm 4.13 checks (Fedora's
# rpm 6 does not), and Debian's mirrors are fast, unlike ALT's from abroad.
set -e
version=$(sh scripts/version)
bin=${BIN_DIR:-target/release}
if ! command -v rpmbuild >/dev/null; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq >/dev/null
    apt-get install -y -qq --no-install-recommends rpm >/dev/null
fi
top=$(mktemp -d)
mkdir -p "$top/SOURCES"
cp "$bin/casement" "$bin/casement-host" "$bin/casement-cli" \
    data/io.github.cheviiot.Casement.desktop data/io.github.cheviiot.Casement.svg \
    data/casement-host.service data/io.github.cheviiot.Casement.metainfo.xml "$top/SOURCES/"
rpmbuild -bb packaging/alt/casement.spec \
    --define "_topdir $top" --define "_sourcedir $top/SOURCES" \
    --define "ver $version" --define "rel ${REL:-alt1}" \
    --define "date_changelog $(LC_ALL=C date '+%a %b %d %Y')"
mkdir -p dist
cp "$top"/RPMS/*/*.rpm dist/
rm -rf "$top"
ls dist/*-alt*.rpm
