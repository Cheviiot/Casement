# Binary package for ALT p11 from the release build (linked in Fedora, checked against
# glibc 2.38 by scripts/check-glibc). Built by packaging/alt/build.sh, not by hand.
%define _unpackaged_files_terminate_build 1
# The binaries are prebuilt; keep them as built.
%set_verify_elf_method relaxed
%add_debuginfo_skiplist %_bindir/*

Name: casement
Version: %ver
Release: %rel
Summary: Remote desktop for the local network and Tailscale
License: MIT
Group: Networking/Remote access
Url: https://github.com/Cheviiot/Casement
ExclusiveArch: x86_64

Source0: casement
Source1: casement-host
Source2: casement-cli
Source3: io.github.cheviiot.Casement.desktop
Source4: io.github.cheviiot.Casement.svg
Source5: casement-host.service
Source6: io.github.cheviiot.Casement.metainfo.xml

# Loaded at run time, so not found from the binaries: capture and playback (base: app,
# opus, audioconvert, volume; good: pulseaudio, autodetect; bad: va, videoparsers,
# openh264) and the PipeWire source for the screen.
Requires: gst-plugins-base1.0 gst-plugins-good1.0 gst-plugins-bad1.0 pipewire

%description
Casement shows and controls another computer over the local network or Tailscale:
screen in H.264, keyboard and mouse, clipboard text, sound, files both ways. The same
application accepts connections to this computer (GNOME on Wayland) through the user
service casement-host.

%install
install -Dm755 %SOURCE0 %buildroot%_bindir/casement
install -Dm755 %SOURCE1 %buildroot%_bindir/casement-host
install -Dm755 %SOURCE2 %buildroot%_bindir/casement-cli
install -Dm644 %SOURCE3 %buildroot%_desktopdir/io.github.cheviiot.Casement.desktop
install -Dm644 %SOURCE4 %buildroot%_iconsdir/hicolor/scalable/apps/io.github.cheviiot.Casement.svg
sed 's|@BINDIR@|%_bindir|' %SOURCE5 > casement-host.service
install -Dm644 casement-host.service %buildroot%_userunitdir/casement-host.service
install -Dm644 %SOURCE6 %buildroot%_datadir/metainfo/io.github.cheviiot.Casement.metainfo.xml

%files
%_bindir/casement
%_bindir/casement-host
%_bindir/casement-cli
%_desktopdir/io.github.cheviiot.Casement.desktop
%_iconsdir/hicolor/scalable/apps/io.github.cheviiot.Casement.svg
%_userunitdir/casement-host.service
%_datadir/metainfo/io.github.cheviiot.Casement.metainfo.xml

%changelog
* %date_changelog Cheviiot <6terio9@vk.com> %ver-%rel
- Casement %ver.
