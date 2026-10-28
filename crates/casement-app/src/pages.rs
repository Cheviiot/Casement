//! Building blocks shared by the application's pages: every screen is a page in the
//! window's navigation stack with its own header and back button, never a dialog on top.

use adw::prelude::*;

/// A page with a header (optionally a button at its end) and scrolled preferences content.
pub fn preferences(
    title: &str,
    tag: &str,
    content: &adw::PreferencesPage,
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

/// The top of a page: a large icon, a title and a line under it, centred.
pub fn hero(icon: &str, title: &str, subtitle: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let image = gtk::Image::builder()
        .icon_name(icon)
        .pixel_size(72)
        .css_classes(["dim-label"])
        .margin_bottom(6)
        .build();
    let heading = gtk::Label::builder()
        .label(title)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["title-1"])
        .build();
    let line = gtk::Label::builder()
        .label(subtitle)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .halign(gtk::Align::Center)
        .margin_top(12)
        .build();
    column.append(&image);
    column.append(&heading);
    column.append(&line);
    line.set_visible(!subtitle.is_empty());
    (column, heading, line)
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
        .css_classes(["pill", style])
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
        .activatable(true)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(icon));
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
        .css_classes(["monospace", "title-3"])
        .build()
}

pub fn placeholder(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::fingerprint_lines;

    #[test]
    fn fingerprint_in_two_lines() {
        assert_eq!(fingerprint_lines("A-B-C-D"), "A-B\nC-D");
        assert_eq!(fingerprint_lines("A-B-C"), "A-B\nC");
        assert_eq!(fingerprint_lines("A"), "A");
    }
}
