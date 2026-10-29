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

/// A stable symbolic icon family for a remote operating system. Old peers have no OS
/// label and keep the neutral computer icon.
pub fn os_icon(os: &str) -> &'static str {
    let os = os.trim().to_ascii_lowercase();
    if os.is_empty() || os.contains("windows") {
        "computer-symbolic"
    } else {
        "drive-harddisk-system-symbolic"
    }
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
    use super::{fingerprint_lines, os_icon};

    #[test]
    fn fingerprint_in_two_lines() {
        assert_eq!(fingerprint_lines("A-B-C-D"), "A-B\nC-D");
        assert_eq!(fingerprint_lines("A-B-C"), "A-B\nC");
        assert_eq!(fingerprint_lines("A"), "A");
    }

    #[test]
    fn operating_system_icons_are_distinct_and_have_a_fallback() {
        assert_eq!(os_icon(""), "computer-symbolic");
        assert_eq!(os_icon("Windows"), "computer-symbolic");
        assert_eq!(os_icon("ALT Workstation"), "drive-harddisk-system-symbolic");
    }
}
