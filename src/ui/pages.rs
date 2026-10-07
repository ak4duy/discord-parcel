use adw::prelude::{ActionRowExt, EntryRowExt, PreferencesGroupExt};
use gtk::{
    glib,
    prelude::{BoxExt, ObjectExt, WidgetExt},
};
use std::path::PathBuf;

use super::widgets::{card, heading, label, page};

pub(super) struct SendPage {
    pub(super) page: gtk::Box,
    pub(super) source_title: gtk::Label,
    pub(super) source_detail: gtk::Label,
    pub(super) pick: gtk::Button,
    pub(super) connection_row: adw::ActionRow,
    pub(super) chunk_size: gtk::SpinButton,
    pub(super) recipient: adw::EntryRow,
    pub(super) scan: gtk::Button,
    pub(super) suggestions: gtk::Box,
    pub(super) encrypt: adw::SwitchRow,
    pub(super) send_password: adw::PasswordEntryRow,
    pub(super) send_button: gtk::Button,
}

impl SendPage {
    pub(super) fn new(chunk_mb: u32) -> Self {
        let send = page();
        send.append(&heading(
            "Send a file",
            "Choose a file and upload it in parts.",
        ));

        let source_title = label("Choose a file to send", "title-3");
        let source_detail = label("Music, video, archives or any file", "dim-label");
        source_detail.set_wrap(true);
        let pick_content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        pick_content.set_margin_top(28);
        pick_content.set_margin_bottom(28);
        pick_content.set_margin_start(20);
        pick_content.set_margin_end(20);
        let texture = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_static(include_bytes!(
            "../../data/parcel.png"
        )))
        .expect("bundled app icon");
        let icon = gtk::Image::from_paintable(Some(&texture));
        icon.set_pixel_size(56);
        pick_content.append(&source_title);
        pick_content.append(&source_detail);
        let pick = gtk::Button::builder()
            .child(&pick_content)
            .css_classes(["file-drop"])
            .build();
        send.append(&pick);
        let send_options = adw::PreferencesGroup::new();
        let connection_row = adw::ActionRow::builder()
            .title("Discord channel")
            .subtitle("Connect your bot to get started")
            .activatable(true)
            .build();
        connection_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        send_options.add(&connection_row);
        let chunk_size = gtk::SpinButton::with_range(1.0, 20.0, 1.0);
        chunk_size.set_value(chunk_mb.clamp(1, 20) as f64);
        chunk_size.set_valign(gtk::Align::Center);
        let chunks = adw::ActionRow::builder()
            .title("Part size")
            .subtitle("MB per attachment")
            .build();
        chunks.add_suffix(&chunk_size);
        send_options.add(&chunks);
        send.append(&send_options);

        let optional_send = adw::PreferencesGroup::builder().title("Optional").build();
        let recipient = adw::EntryRow::builder().title("Ping when done").build();
        let scan = gtk::Button::with_label("Scan people");
        scan.set_valign(gtk::Align::Center);
        recipient.add_suffix(&scan);
        optional_send.add(&recipient);

        let suggestions = gtk::Box::new(gtk::Orientation::Vertical, 6);
        suggestions.set_visible(false);
        optional_send.add(&suggestions);

        let encrypt = adw::SwitchRow::builder()
            .title("Encrypt before sending")
            .subtitle("Protect file contents with a shared passphrase")
            .build();
        let send_password = adw::PasswordEntryRow::builder()
            .title("Encryption passphrase")
            .sensitive(false)
            .build();
        optional_send.add(&encrypt);
        optional_send.add(&send_password);
        encrypt
            .bind_property("active", &send_password, "sensitive")
            .sync_create()
            .build();
        send.append(&optional_send);

        let send_button = gtk::Button::with_label("Send");
        send_button.add_css_class("suggested-action");
        send_button.add_css_class("pill");
        send_button.set_halign(gtk::Align::Center);
        send_button.set_sensitive(false);
        send.append(&send_button);
        Self {
            page: send,
            source_title,
            source_detail,
            pick,
            connection_row,
            chunk_size,
            recipient,
            scan,
            suggestions,
            encrypt,
            send_password,
            send_button,
        }
    }
}

pub(super) struct ReceivePage {
    pub(super) page: gtk::Box,
    pub(super) receive_file: adw::ActionRow,
    pub(super) receive_link: adw::EntryRow,
    pub(super) decrypt: adw::SwitchRow,
    pub(super) receive_password: adw::PasswordEntryRow,
    pub(super) destination: PathBuf,
    pub(super) destination_row: adw::ActionRow,
    pub(super) receive_button: gtk::Button,
}

impl ReceivePage {
    pub(super) fn new() -> Self {
        let receive = page();
        receive.append(&heading(
            "Receive a file",
            "Open a transfer file or paste a Discord message link.",
        ));
        let receive_options = adw::PreferencesGroup::new();
        let receive_file = adw::ActionRow::builder()
            .title("Open transfer file")
            .activatable(true)
            .build();
        receive_file.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
        receive_options.add(&receive_file);
        let receive_link = adw::EntryRow::builder()
            .title("Or paste a Discord message link")
            .build();
        receive_options.add(&receive_link);
        receive.append(&receive_options);

        let optional_receive = adw::PreferencesGroup::builder().title("Optional").build();
        let decrypt = adw::SwitchRow::builder()
            .title("Decrypt encrypted parcel")
            .subtitle("Use the passphrase shared by the sender")
            .build();
        let receive_password = adw::PasswordEntryRow::builder()
            .title("Decryption passphrase")
            .sensitive(false)
            .build();
        optional_receive.add(&decrypt);
        optional_receive.add(&receive_password);
        decrypt
            .bind_property("active", &receive_password, "sensitive")
            .sync_create()
            .build();

        let destination_group = adw::PreferencesGroup::new();
        let home = glib::home_dir();
        let destination = glib::user_special_dir(glib::UserDirectory::Downloads)
            .filter(|p| p.is_dir())
            .unwrap_or(home);
        let destination_row = adw::ActionRow::builder()
            .title("Save restored file to")
            .subtitle(destination.to_string_lossy())
            .activatable(true)
            .build();
        destination_row.add_prefix(&gtk::Image::from_icon_name("folder-download-symbolic"));
        destination_row.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
        destination_group.add(&destination_row);
        receive.append(&destination_group);
        receive.append(&optional_receive);

        let receive_button = gtk::Button::with_label("Download and Restore");
        receive_button.add_css_class("suggested-action");
        receive_button.add_css_class("pill");
        receive_button.set_halign(gtk::Align::Center);
        receive.append(&receive_button);
        Self {
            page: receive,
            receive_file,
            receive_link,
            decrypt,
            receive_password,
            destination,
            destination_row,
            receive_button,
        }
    }
}

pub(super) struct TransferProgress {
    pub(super) progress_box: gtk::Box,
    pub(super) progress_title: gtk::Label,
    pub(super) progress_detail: gtk::Label,
    pub(super) progress_bar: gtk::ProgressBar,
    pub(super) spinner: gtk::Spinner,
    pub(super) cancel_button: gtk::Button,
}

impl TransferProgress {
    pub(super) fn new() -> Self {
        let progress_box = card();
        progress_box.set_visible(false);
        let progress_top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let spinner = gtk::Spinner::new();
        progress_top.append(&spinner);
        let progress_title = label("Preparing transfer", "heading");
        progress_title.set_hexpand(true);
        progress_title.set_xalign(0.0);
        progress_title.set_wrap(true);
        progress_top.append(&progress_title);
        let cancel_button = gtk::Button::with_label("Stop");
        progress_top.append(&cancel_button);
        progress_box.append(&progress_top);
        let progress_bar = gtk::ProgressBar::new();
        progress_box.append(&progress_bar);
        let progress_detail = label("", "dim-label");
        progress_detail.set_xalign(0.0);
        progress_box.append(&progress_detail);
        Self {
            progress_box,
            progress_title,
            progress_detail,
            progress_bar,
            spinner,
            cancel_button,
        }
    }
}
