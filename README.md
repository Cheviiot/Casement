<div align="center">

<img src="data/io.github.cheviiot.Casement.svg" width="96" alt="">

# Casement

**Remote desktop for your own network — local or Tailscale.**

[Русский](README.ru.md) · English

[![Build](https://github.com/Cheviiot/Casement/actions/workflows/build.yml/badge.svg)](https://github.com/Cheviiot/Casement/actions/workflows/build.yml)
[![Release](https://img.shields.io/github/v/release/Cheviiot/Casement)](https://github.com/Cheviiot/Casement/releases/latest)
[![MIT](https://img.shields.io/badge/license-MIT-2ea44f.svg)](LICENSE)

<img src="docs/images/home.png" width="760" alt="Casement in the light and dark styles">

[![Windows](https://img.shields.io/badge/Windows-installer-0078D4?style=for-the-badge)](https://github.com/Cheviiot/Casement/releases/latest/download/casement-windows-x86_64-setup.exe)
[![ALT](https://img.shields.io/badge/ALT_p11-rpm-F5A623?style=for-the-badge)](https://github.com/Cheviiot/Casement/releases/latest/download/casement-alt-p11.x86_64.rpm)
[![Fedora](https://img.shields.io/badge/Fedora-rpm-51A2DA?style=for-the-badge)](https://github.com/Cheviiot/Casement/releases/latest/download/casement-fedora.x86_64.rpm)
[![Debian](https://img.shields.io/badge/Debian_|_Ubuntu-deb-A81D33?style=for-the-badge)](https://github.com/Cheviiot/Casement/releases/latest/download/casement_amd64.deb)
[![Flatpak](https://img.shields.io/badge/Flatpak-any_Linux-4A90D9?style=for-the-badge)](https://github.com/Cheviiot/Casement/releases/latest/download/casement-x86_64.flatpak)

</div>

## Why Casement

- **Direct.** Computers talk to each other over your LAN or Tailscale — no relay
  servers, no accounts, no cloud.
- **Fast.** Hardware H.264 over QUIC: about 26 ms from a mouse move to the new frame on a
  home network.
- **Complete.** Screen, mouse and keyboard, sound, clipboard text, files both ways,
  several monitors, automatic reconnect.
- **Private.** TLS 1.3 with a key per device; the access password is checked with OPAQUE
  and never leaves your computer.
- **Native.** One GTK 4 / Libadwaita application for Linux and Windows that both connects
  and lets others in.

> The interface is in Russian for now.

## Get started

1. Install Casement on both computers. Accepting connections is on by default.
2. On the computer you will connect to, set an access password: **Этот компьютер →
   Пароль доступа**, or `casement-cli setup` in a terminal.
3. On your computer, pick it under **В этой сети**, or type its address or Tailscale
   name. Enter the password once; from then on the device is trusted.

## Without a desktop

```sh
casement-cli setup        # step-by-step: password, accepting connections, trusted devices
casement-cli status       # how this computer is set up
casement-cli accept off   # stop accepting connections
```

## Requirements

- **Windows** 10 or 11. Casement runs as a service, so it also shows the login screen,
  the lock screen and UAC prompts.
- **Linux** with GTK 4.14+, Libadwaita 1.6+ and GStreamer 1.24+ (the packages pull them
  in). Accepting connections needs GNOME on Wayland.

Every file is in the [latest release](https://github.com/Cheviiot/Casement/releases/latest),
including a portable Windows archive and `install.sh` for a per-user install on any
distribution.

## More

[How it works](docs/ARCHITECTURE.md) · [Building and checks](docs/DEVELOPMENT.md) ·
[MIT license](LICENSE)
