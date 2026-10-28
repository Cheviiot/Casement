# Casement — rules for agents

Casement is a remote desktop for the LAN and Tailscale, all Rust: a QUIC network core and
a GTK 4 / Libadwaita application through gtk4-rs and libadwaita-rs, the same on Linux and
Windows, direct connections only. Read `docs/ARCHITECTURE.md` and `docs/DEVELOPMENT.md`
first; the machine-wide rules in `~/.claude/CLAUDE.md` apply. UI text is Russian; code,
comments and commit messages are English.

- Own code only. RustDesk and similar AGPL projects are a reference for behaviour, never
  for code or protocol definitions. Dependencies must be permissive (MIT, Apache, BSD,
  ISC, Zlib, Unicode; LGPL for GStreamer and GTK themselves; MPL-2.0 only for unmodified
  crates such as gst-plugin-gtk4); check before adding one.
- No RDP, SSH or Vala: they were removed on purpose.
- Every screen of the application is a page of the window's navigation stack (no
  dialogs or popovers); texts are short: a title and a state, no explanations.
- Build and test on Fedora as in `docs/DEVELOPMENT.md`; on the owner's machine inside
  Distrobox `oriel-dev` through `scripts/dev/in-box`, never on the host. Heavy steps at
  low priority with `-j 4`.
- Binaries must start on ALT p11 (glibc 2.38) although they are linked in Fedora: keep
  `scripts/check-glibc` passing and `casement-net/src/glibc_compat.rs` in place; never use
  std's unstable pidfd API.
- Threads: networking runs on the Tokio runtime (`casement-app/src/core.rs`); GTK objects
  stay on the main thread. Await network work with `core::run`, pass streams with
  async-channel, never block the main loop.
- Screen on GNOME goes through Mutter's RemoteDesktop/ScreenCast D-Bus API (no portal
  dialog). Do not send keyboard input to the owner's live sessions in tests; pointer moves
  are fine. Never show test windows on the owner's desktop: use `scripts/dev/ui-check`.
- Security: the device key never leaves `device.key` (0600) and is never logged;
  passwords are never stored in plain text or logged. A new computer's key is remembered
  without asking (owner's decision); a changed key of a known computer must keep stopping
  the connection.
- Tests must not need a network peer beyond loopback. CI (`.github/workflows/build.yml`)
  must stay green: fmt, clippy with `-D warnings`, tests, Linux, ALT and Windows builds.
- Releases: bump the version in `Cargo.toml`, tag `vX.Y.Z`, push the tag; CI publishes.
