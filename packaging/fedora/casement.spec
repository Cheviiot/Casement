# Binary package for Fedora from the release build. Built by packaging/fedora/build.sh,
# not by hand.
%global debug_package %{nil}

Name:           casement
Version:        %{ver}
Release:        %{rel}%{?dist}
Summary:        Remote desktop for the local network and Tailscale
License:        MIT
URL:            https://github.com/Cheviiot/Casement
ExclusiveArch:  x86_64

Source0:        casement
Source1:        casement-host
Source2:        casement-cli
Source3:        io.github.cheviiot.Casement.desktop
Source4:        io.github.cheviiot.Casement.svg
Source5:        casement-host.service
Source6:        LICENSE
Source7:        io.github.cheviiot.Casement.metainfo.xml

# Loaded at run time, so not found from the binaries: capture, encoding, decoding and
# playback plugins, and the PipeWire source for the screen.
Requires:       gstreamer1-plugins-base
Requires:       gstreamer1-plugins-good
Requires:       gstreamer1-plugins-bad-free
Requires:       pipewire-gstreamer
Recommends:     gstreamer1-plugin-openh264

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
sed 's|@BINDIR@|%{_bindir}|' %{SOURCE5} > casement-host.service
install -Dm644 casement-host.service %{buildroot}%{_userunitdir}/casement-host.service
install -Dm644 %{SOURCE6} %{buildroot}%{_licensedir}/%{name}/LICENSE
install -Dm644 %{SOURCE7} %{buildroot}%{_metainfodir}/io.github.cheviiot.Casement.metainfo.xml

%files
%license %{_licensedir}/%{name}/LICENSE
%{_bindir}/casement
%{_bindir}/casement-host
%{_bindir}/casement-cli
%{_datadir}/applications/io.github.cheviiot.Casement.desktop
%{_datadir}/icons/hicolor/scalable/apps/io.github.cheviiot.Casement.svg
%{_userunitdir}/casement-host.service
%{_metainfodir}/io.github.cheviiot.Casement.metainfo.xml

%changelog
* %{date_changelog} Cheviiot <6terio9@vk.com> - %{ver}-%{rel}
- Casement %{ver}.
