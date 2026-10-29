//! Building blocks shared by the application's pages: every screen is a page in the
//! window's navigation stack with its own header and back button, never a dialog on top.

use adw::prelude::*;

/// Short Russian units even when the system locale has no GLib translations.
pub fn file_size(bytes: u64) -> String {
    let units = ["Б", "КиБ", "МиБ", "ГиБ", "ТиБ", "ПиБ", "ЭиБ"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} Б")
    } else {
        format!("{size:.1} {}", units[unit]).replace('.', ",")
    }
}

/// A page with a header (optionally a button at its end) and scrolled preferences content.
pub fn preferences(
    title: &str,
    tag: &str,
    content: &adw::PreferencesPage,
    header_end: Option<&gtk::Widget>,
) -> adw::NavigationPage {
    content.add_css_class("casement-preferences");
    frame(title, tag, content, header_end)
}

/// The same chrome for forms, browsers and status pages. The title is shown once.
pub fn frame(
    title: &str,
    tag: &str,
    content: &impl IsA<gtk::Widget>,
    header_end: Option<&gtk::Widget>,
) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    if let Some(widget) = header_end {
        header.pack_end(widget);
    }
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(content));
    adw::NavigationPage::builder()
        .title(title)
        .tag(tag)
        .child(&view)
        .build()
}

/// A restrained page identity; callers use a different page title, or omit this block.
pub fn hero(icon: &str, title: &str, subtitle: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let image = icon_tile(icon);
    let heading = gtk::Label::builder()
        .label(title)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["title-2"])
        .build();
    let line = gtk::Label::builder()
        .label(subtitle)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    column.append(&heading);
    column.append(&line);
    let identity = gtk::Box::builder()
        .spacing(16)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    identity.append(&image);
    identity.append(&column);
    line.set_visible(!subtitle.is_empty());
    (identity, heading, line)
}

pub fn icon_tile(icon: &str) -> gtk::Image {
    gtk::Image::builder()
        .icon_name(icon)
        .pixel_size(24)
        .width_request(48)
        .height_request(48)
        .valign(gtk::Align::Center)
        .css_classes(["device-icon"])
        .build()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteOs {
    Windows,
    AltLinux,
    Ubuntu,
    Debian,
    Fedora,
    Arch,
    OpenSuse,
    Mint,
    NixOs,
    Linux,
    Unknown,
}

fn remote_os(os: &str) -> RemoteOs {
    let os = os.trim().to_ascii_lowercase();
    if os.contains("windows") {
        RemoteOs::Windows
    } else if os.starts_with("alt ") || os.contains("alt linux") {
        RemoteOs::AltLinux
    } else if os.contains("ubuntu") {
        RemoteOs::Ubuntu
    } else if os.contains("debian") {
        RemoteOs::Debian
    } else if os.contains("fedora") {
        RemoteOs::Fedora
    } else if os.contains("arch linux") || os.starts_with("arch ") {
        RemoteOs::Arch
    } else if os.contains("opensuse") || os.contains("open suse") {
        RemoteOs::OpenSuse
    } else if os.contains("linux mint") {
        RemoteOs::Mint
    } else if os.contains("nixos") || os.contains("nix os") {
        RemoteOs::NixOs
    } else if os.contains("linux") {
        RemoteOs::Linux
    } else {
        RemoteOs::Unknown
    }
}

/// Make the vendored icon theme visible while running directly from the source tree.
/// Installed packages place the same files in the normal hicolor theme.
#[cfg(debug_assertions)]
pub fn register_source_icons() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/icons");
    if path.is_dir() {
        gtk::IconTheme::for_display(&display).add_search_path(path);
    }
}

#[cfg(not(debug_assertions))]
pub fn register_source_icons() {}

fn remote_icon_name(os: RemoteOs, theme: &gtk::IconTheme) -> &'static str {
    let preferred = match os {
        RemoteOs::Windows => "casement-os-windows",
        // ALT's local icon is an old text-heavy CC-BY-SA logo that becomes unreadable
        // at card size. Devicon has no ALT mark, so use its clean generic Linux icon.
        RemoteOs::AltLinux => "casement-os-linux-symbolic",
        RemoteOs::Ubuntu => "casement-os-ubuntu",
        RemoteOs::Debian => "casement-os-debian",
        RemoteOs::Fedora => "casement-os-fedora",
        RemoteOs::Arch => "casement-os-arch",
        RemoteOs::OpenSuse => "casement-os-opensuse",
        RemoteOs::Mint => "casement-os-mint",
        RemoteOs::NixOs => "casement-os-nixos",
        RemoteOs::Linux => "casement-os-linux-symbolic",
        RemoteOs::Unknown => "computer-symbolic",
    };
    if theme.has_icon(preferred) {
        preferred
    } else if !matches!(os, RemoteOs::Windows | RemoteOs::Unknown)
        && theme.has_icon("casement-os-linux-symbolic")
    {
        "casement-os-linux-symbolic"
    } else {
        "computer-symbolic"
    }
}

/// A maintained OS or distribution mark from the bundled Devicon subset. Distros not
/// represented there, including ALT Linux, use the generic Linux mark.
pub fn os_icon_tile(os: &str) -> gtk::Box {
    let os = remote_os(os);
    let class = match os {
        RemoteOs::Windows => "os-windows",
        RemoteOs::AltLinux
        | RemoteOs::Ubuntu
        | RemoteOs::Debian
        | RemoteOs::Fedora
        | RemoteOs::Arch
        | RemoteOs::OpenSuse
        | RemoteOs::Mint
        | RemoteOs::NixOs
        | RemoteOs::Linux => "os-linux",
        RemoteOs::Unknown => "os-unknown",
    };
    let theme = gtk::IconTheme::for_display(&gtk::gdk::Display::default().expect("display"));
    let image = gtk::Image::builder()
        .icon_name(remote_icon_name(os, &theme))
        .pixel_size(30)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    let tile = gtk::Box::builder()
        .width_request(56)
        .height_request(56)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["os-tile", class])
        .build();
    tile.append(&image);
    tile
}

/// Consistent title and metadata for home and LAN discovery cards.
pub fn connection_labels(name: &str, address: &str, os: &str) -> (String, String) {
    let name = name.trim();
    let os = os.trim();
    let title = if name.is_empty() { address } else { name }.to_string();
    let subtitle = match (name.is_empty(), os.is_empty()) {
        (true, true) => String::new(),
        (true, false) => os.to_string(),
        (false, true) => address.to_string(),
        (false, false) => format!("{os}  ·  {address}"),
    };
    (title, subtitle)
}

pub fn scrolled(content: &impl IsA<gtk::Widget>, maximum: i32) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(maximum)
        .tightening_threshold(560)
        .child(content)
        .margin_top(24)
        .margin_bottom(32)
        .margin_start(18)
        .margin_end(18)
        .valign(gtk::Align::Start)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .vexpand(true)
        .build()
}

/// A preferences group holding one widget, without a frame: heroes and big buttons.
pub fn bare(widget: &impl IsA<gtk::Widget>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.add(widget);
    group
}

/// A large rounded button for the page's main action.
pub fn pill(label: &str, style: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .halign(gtk::Align::Center)
        .css_classes(["primary-action", style])
        .build()
}

pub fn flat_icon(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build()
}

/// A row that opens another page.
pub fn link_row(icon: &str, title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(title).as_str())
        .subtitle(glib::markup_escape_text(subtitle).as_str())
        .title_lines(1)
        .subtitle_lines(1)
        .activatable(true)
        .build();
    row.add_prefix(&icon_tile(icon));
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

/// A fingerprint as people read it aloud: its groups in two lines.
fn fingerprint_lines(display: &str) -> String {
    let groups: Vec<&str> = display.split('-').collect();
    let half = groups.len().div_ceil(2);
    format!("{}\n{}", groups[..half].join("-"), groups[half..].join("-"))
        .trim()
        .to_string()
}

pub fn fingerprint_label(display: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(fingerprint_lines(display))
        .justify(gtk::Justification::Center)
        .css_classes(["monospace", "fingerprint"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::{connection_labels, fingerprint_lines, remote_os, RemoteOs};

    #[test]
    fn fingerprint_in_two_lines() {
        assert_eq!(fingerprint_lines("A-B-C-D"), "A-B\nC-D");
        assert_eq!(fingerprint_lines("A-B-C"), "A-B\nC");
        assert_eq!(fingerprint_lines("A"), "A");
    }

    #[test]
    fn operating_system_icons_are_distinct_and_have_a_fallback() {
        assert_eq!(remote_os(""), RemoteOs::Unknown);
        assert_eq!(remote_os("Windows 11"), RemoteOs::Windows);
        assert_eq!(remote_os("ALT Atomic Onyx"), RemoteOs::AltLinux);
        assert_eq!(remote_os("Fedora Linux"), RemoteOs::Fedora);
        assert_eq!(remote_os("Ubuntu 26.04"), RemoteOs::Ubuntu);
        assert_eq!(remote_os("Debian GNU/Linux"), RemoteOs::Debian);
        assert_eq!(remote_os("Arch Linux"), RemoteOs::Arch);
        assert_eq!(remote_os("openSUSE Tumbleweed"), RemoteOs::OpenSuse);
        assert_eq!(remote_os("Linux Mint"), RemoteOs::Mint);
        assert_eq!(remote_os("NixOS"), RemoteOs::NixOs);
    }

    #[test]
    fn connection_card_keeps_os_and_address_out_of_the_title() {
        assert_eq!(
            connection_labels("GEM12", "gem12.local", "Windows 11"),
            (
                "GEM12".to_string(),
                "Windows 11  ·  gem12.local".to_string()
            )
        );
        assert_eq!(
            connection_labels("", "192.168.1.20", "ALT Linux"),
            ("192.168.1.20".to_string(), "ALT Linux".to_string())
        );
    }
}
