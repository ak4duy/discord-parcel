use gtk::prelude::{BoxExt, WidgetExt};

pub(super) fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label
}

pub(super) fn heading(title: &str, subtitle: &str) -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
    box_.set_margin_top(12);
    box_.set_margin_bottom(8);
    let title = label(title, "title-1");
    title.set_wrap(true);
    title.set_justify(gtk::Justification::Center);
    let subtitle = label(subtitle, "dim-label");
    subtitle.set_wrap(true);
    subtitle.set_justify(gtk::Justification::Center);
    box_.append(&title);
    box_.append(&subtitle);
    box_
}

pub(super) fn page() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, 18)
}

pub(super) fn card() -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 12);
    box_.add_css_class("card");
    box_.add_css_class("transfer-card");
    box_
}
