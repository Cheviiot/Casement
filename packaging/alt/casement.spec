# Binary package for ALT p11 from the release build, made by packaging/alt/build.sh with
# Debian's rpmbuild. ALT's rpm 4.13 reads plain v4 packages with an xz payload, so no ALT
# system is needed; the dependencies are listed by ALT package name instead of
# being found from the binaries, which would need the libraries installed.
%global debug_package %{nil}
%define _rpmformat 4
%define _binary_payload w9.xzdio
%define _binary_filedigest_algorithm 8

Name: casement
Version: %{ver}
Release: %{rel}
Summary: Remote desktop for the local network and Tailscale
License: MIT
Group: Networking/Remote access
Url: https://github.com/Cheviiot/Casement
ExclusiveArch: x86_64
AutoReqProv: no

Source0: casement
Source1: casement-host
Source2: casement-cli
Source3: io.github.cheviiot.Casement.desktop
Source4: io.github.cheviiot.Casement.svg
Source5: casement-host.service
Source6: io.github.cheviiot.Casement.metainfo.xml
Source7: casement-os-icons.tar.gz
Source8: LICENSE.devicon

# Linked libraries (the binaries need glibc 2.38 or older, checked by check-glibc).
Requires: glibc-core >= 2.38
Requires: libgcc1
Requires: glib2
Requires: libgio
Requires: libgraphene
Requires: libgtk4 >= 4.14
Requires: libadwaita >= 1.6
Requires: libgstreamer1.0 >= 1.24
Requires: libgst-plugins1.0 >= 1.24
# Loaded at run time: capture, encoding, decoding and playback plugins, and the PipeWire
# source for the screen.
Requires: gst-plugins-base1.0
Requires: gst-plugins-good1.0
Requires: gst-plugins-bad1.0
Requires: pipewire

%description
Casement shows and controls another computer over the local network or Tailscale:
screen in H.264, keyboard and mouse, clipboard text, sound, files both ways. The same
application accepts connections to this computer (GNOME on Wayland) through the user
service casement-host.

%install
install -Dm755 %{SOURCE0} %{buildroot}%{_bindir}/casement
install -Dm755 %{SOURCE1} %{buildroot}%{_bindir}/casement-host
install -Dm755 %{SOURCE2} %{buildroot}%{_bindir}/casement-cli
install -Dm644 %{SOURCE3} %{buildroot}%{_datadir}/applications/io.github.cheviiot.Casement.desktop
install -Dm644 %{SOURCE4} %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.github.cheviiot.Casement.svg
tar -xzf %{SOURCE7} -C %{buildroot}%{_datadir}/icons/hicolor/scalable/apps
sed 's|@BINDIR@|%{_bindir}|' %{SOURCE5} > casement-host.service
install -Dm644 casement-host.service %{buildroot}/usr/lib/systemd/user/casement-host.service
install -Dm644 %{SOURCE6} %{buildroot}%{_datadir}/metainfo/io.github.cheviiot.Casement.metainfo.xml
install -Dm644 %{SOURCE8} %{buildroot}%{_datadir}/licenses/%{name}/LICENSE.devicon


%post
# Accepting connections is on by default, for every user from their next login.
systemctl --global enable casement-host.service >/dev/null 2>&1 || :

%preun
if [ "$1" = 0 ]; then
    systemctl --global disable casement-host.service >/dev/null 2>&1 || :
fi

%files
%{_bindir}/casement
%{_bindir}/casement-host
%{_bindir}/casement-cli
%{_datadir}/applications/io.github.cheviiot.Casement.desktop
%{_datadir}/icons/hicolor/scalable/apps/io.github.cheviiot.Casement.svg
%{_datadir}/icons/hicolor/scalable/apps/casement-os-*.svg
/usr/share/licenses/%{name}/LICENSE.devicon
/usr/lib/systemd/user/casement-host.service
%{_datadir}/metainfo/io.github.cheviiot.Casement.metainfo.xml

%changelog
* %{date_changelog} Cheviiot <6terio9@vk.com> %{ver}-%{rel}
- Casement %{ver}.
