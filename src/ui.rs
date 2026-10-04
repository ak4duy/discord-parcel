use adw::prelude::*;
use anyhow::Result;
use discord_parcel::{
    discord::Discord,
    parcel::{self, Cancel, Manifest, Progress},
    transfer::{self, Connection},
};
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    channel_id: String,
    #[serde(default)]
    token: String,
    #[serde(default = "default_chunk", alias = "chunk_mib")]
    chunk_mb: u32,
}

fn default_chunk() -> u32 {
    20
}

struct Outcome {
    title: String,
    detail: String,
    file: Option<PathBuf>,
    link: Option<String>,
}

enum Event {
    Progress(Progress),
    Finished(std::result::Result<Outcome, String>),
}

struct Ui {
    window: adw::ApplicationWindow,
    toast: adw::ToastOverlay,
    controls: gtk::Box,
    connection_button: gtk::Button,
    source: RefCell<Option<PathBuf>>,
    parcel_file: RefCell<Option<PathBuf>>,
    destination: RefCell<PathBuf>,
    connection: RefCell<Connection>,
    chunk_size: gtk::SpinButton,
    channel_name: RefCell<Option<String>>,
    recipient: adw::EntryRow,
    suggestions: gtk::Box,
    participants: RefCell<Vec<(String, String)>>,
    encrypt: adw::SwitchRow,
    send_password: adw::PasswordEntryRow,
    decrypt: adw::SwitchRow,
    receive_password: adw::PasswordEntryRow,
    source_title: gtk::Label,
    source_detail: gtk::Label,
    receive_file: adw::ActionRow,
    receive_link: adw::EntryRow,
    destination_row: adw::ActionRow,
    connection_row: adw::ActionRow,
    send_button: gtk::Button,

    stack: adw::ViewStack,
    progress_box: gtk::Box,
    progress_title: gtk::Label,
    progress_detail: gtk::Label,
    progress_bar: gtk::ProgressBar,
    spinner: gtk::Spinner,
    cancel_button: gtk::Button,
    busy: Cell<bool>,
    cancel: RefCell<Cancel>,
}

pub fn present(app: &adw::Application, initial: Option<PathBuf>) {
    if initial.is_none()
        && let Some(window) = app.active_window()
    {
        window.present();
        return;
    }
    let css = gtk::CssProvider::new();
    css.load_from_string(include_str!("../data/style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let settings: Settings =
        parcel::read_bounded(&parcel::data_dir().join("settings.json"), 32 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Settings {
                channel_id: String::new(),
                token: String::new(),
                chunk_mb: default_chunk(),
            });
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Discord Parcel")
        .default_width(780)
        .default_height(830)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new("Discord Parcel", "");
    header.set_title_widget(Some(&title));
    let connection_button = gtk::Button::from_icon_name("network-server-symbolic");
    connection_button.set_tooltip_text(Some("Connection settings"));
    header.pack_start(&connection_button);
    let menu = gio::Menu::new();
    menu.append(Some("About"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .build();
    header.pack_end(&menu_button);
    toolbar.add_top_bar(&header);

    let stack = adw::ViewStack::new();
    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .halign(gtk::Align::Center)
        .build();
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 12);
    controls.append(&switcher);
    controls.append(&stack);
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
        "../data/parcel.png"
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
    chunk_size.set_value(settings.chunk_mb.clamp(1, 20) as f64);
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
    stack.add_titled_with_icon(&send, Some("send"), "Send", "document-send-symbolic");

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

    stack.add_titled_with_icon(
        &receive,
        Some("receive"),
        "Receive",
        "folder-download-symbolic",
    );

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

    let content = gtk::Box::new(gtk::Orientation::Vertical, 20);
    content.set_margin_top(12);
    content.set_margin_bottom(28);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(&controls);
    content.append(&progress_box);
    let clamp = adw::Clamp::builder()
        .maximum_size(660)
        .tightening_threshold(520)
        .child(&content)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .vexpand(true)
        .build();
    let toast = adw::ToastOverlay::new();
    toast.set_child(Some(&scroll));
    toolbar.set_content(Some(&toast));
    window.set_content(Some(&toolbar));

    let ui = Rc::new(Ui {
        window,
        toast,
        controls,
        connection_button,
        source: RefCell::new(None),
        parcel_file: RefCell::new(None),
        destination: RefCell::new(destination),
        connection: RefCell::new(Connection {
            token: std::env::var("DISCORD_BOT_TOKEN").unwrap_or(settings.token),
            channel_id: settings.channel_id,
        }),
        chunk_size,
        channel_name: RefCell::new(None),
        recipient,
        suggestions,
        participants: RefCell::new(Vec::new()),
        encrypt,
        send_password,
        decrypt,
        receive_password,
        source_title,
        source_detail,
        receive_file,
        receive_link,
        destination_row,
        connection_row,
        send_button,

        stack,
        progress_box,
        progress_title,
        progress_detail,
        progress_bar,
        spinner,
        cancel_button,
        busy: Cell::new(false),
        cancel: RefCell::new(Cancel::default()),
    });
    ui.refresh_connection();
    ui.scan_connection();
    {
        let copy = ui.clone();
        scan.connect_clicked(move |_| copy.scan_connection());
    }
    {
        let copy = ui.clone();
        ui.recipient
            .connect_changed(move |_| copy.refresh_suggestions());
    }
    {
        let copy = ui.clone();
        ui.encrypt
            .connect_active_notify(move |_| copy.refresh_source());
    }
    {
        let ui = ui.clone();
        pick.connect_clicked(move |_| ui.choose_source());
    }
    let drop = gtk::DropTarget::new(gio::File::static_type(), gtk::gdk::DragAction::COPY);
    {
        let ui = ui.clone();
        drop.connect_drop(move |_, value, _, _| {
            if ui.busy.get() {
                return false;
            }
            if let Ok(file) = value.get::<gio::File>()
                && let Some(path) = file.path()
            {
                ui.set_source(path);
                return true;
            }
            false
        });
    }
    pick.add_controller(drop);
    {
        let copy = ui.clone();
        ui.connection_button
            .connect_clicked(move |_| copy.connection_dialog());
    }
    {
        let copy = ui.clone();
        ui.connection_row
            .connect_activated(move |_| copy.connection_dialog());
    }
    {
        let copy = ui.clone();
        ui.chunk_size.connect_value_changed(move |_| {
            copy.refresh_source();
            copy.save_settings();
        });
    }
    {
        let copy = ui.clone();
        ui.send_button.connect_clicked(move |_| copy.send());
    }

    {
        let copy = ui.clone();
        ui.receive_file
            .connect_activated(move |_| copy.choose_manifest());
    }
    {
        let copy = ui.clone();
        ui.receive_link.connect_changed(move |entry| {
            if !entry.text().trim().is_empty() {
                copy.parcel_file.replace(None);
                copy.receive_file.set_subtitle("Choose a .parcel.json file");
            }
        });
    }
    {
        let copy = ui.clone();
        ui.destination_row
            .connect_activated(move |_| copy.choose_destination());
    }
    {
        let copy = ui.clone();
        receive_button.connect_clicked(move |_| copy.receive());
    }

    {
        let copy = ui.clone();
        ui.cancel_button.connect_clicked(move |_| {
            copy.cancel.borrow().cancel();
            copy.cancel_button.set_sensitive(false);
            copy.progress_title
                .set_label("Stopping after the current request…");
        });
    }
    {
        let copy = ui.clone();
        ui.window.connect_close_request(move |_| {
            if copy.busy.get() {
                copy.toast(
                    "Stop the transfer before closing. Completed Discord parts are saved for resume.",
                );
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    let about = gio::SimpleAction::new("about", None);
    {
        let copy = ui.clone();
        about.connect_activate(move |_, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Discord Parcel")
                .application_icon("dev.akaduy.DiscordParcel")
                .developer_name("akaduy")
                .version(env!("CARGO_PKG_VERSION"))
                .copyright("Copyright (c) 2026 ak4duy")
                                .license_type(gtk::License::Gpl30Only)
                .comments(
                    "Split, send, and restore files through Discord\nBuilt with Rust, GTK4, and libadwaita.",
                )
                .build();
            dialog.present(Some(&copy.window));
        });
    }
    ui.window.add_action(&about);
    if let Some(path) = initial {
        if path.to_string_lossy().ends_with(".parcel.json") {
            ui.set_manifest(path);
        } else {
            ui.set_source(path);
        }
    }
    ui.window.present();
}

impl Ui {
    fn toast(&self, text: &str) {
        self.toast.add_toast(adw::Toast::new(text));
    }

    fn save_settings(&self) {
        let settings = Settings {
            channel_id: self.connection.borrow().channel_id.clone(),
            token: self.connection.borrow().token.clone(),
            chunk_mb: self.chunk_size.value_as_int() as u32,
        };
        let result = serde_json::to_vec_pretty(&settings)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| {
                parcel::atomic_write(&parcel::data_dir().join("settings.json"), &bytes)
            });
        if let Err(error) = result {
            self.toast(&format!("Could not save preferences: {error}"));
        }
    }

    fn refresh_connection(&self) {
        let connection = self.connection.borrow();
        let subtitle = if connection.token.trim().is_empty() {
            "Connect your bot to get started".to_owned()
        } else if connection.channel_id.is_empty() {
            "Choose a channel".to_owned()
        } else {
            self.channel_name
                .borrow()
                .as_ref()
                .map(|name| format!("Connected to the #{name}"))
                .unwrap_or_else(|| "Checking channel connection…".into())
        };
        self.connection_row.set_subtitle(&subtitle);
    }

    fn scan_connection(self: &Rc<Self>) {
        let connection = self.connection.borrow().clone();
        if connection.token.is_empty() || connection.channel_id.is_empty() {
            return;
        }

        let (sender, receiver) = async_channel::bounded(1);
        let expected = connection.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<_> {
                let discord = Discord::new(&connection.token)?;
                let cancel = Cancel::default();
                let channel = discord.channel(&connection.channel_id, &cancel)?;
                let people = discord.participants(&connection.channel_id, &cancel);
                Ok((channel.name, people.map_err(|error| error.to_string())))
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send_blocking(result);
        });

        let ui = self.clone();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            if ui.connection.borrow().channel_id != expected.channel_id
                || ui.connection.borrow().token != expected.token
            {
                return;
            }
            match result {
                Ok((name, people)) => {
                    ui.channel_name.replace(Some(name));
                    ui.refresh_connection();
                    match people {
                        Ok(people) => {
                            ui.participants.replace(people);
                        }
                        Err(_) => ui
                            .toast("Could not scan people. You can still paste a Discord user ID."),
                    }
                    ui.refresh_suggestions();
                }
                Err(error) => {
                    ui.connection_row
                        .set_subtitle("Connection failed · check settings");
                    ui.toast(&error);
                }
            }
        });
    }

    fn refresh_suggestions(self: &Rc<Self>) {
        while let Some(child) = self.suggestions.first_child() {
            self.suggestions.remove(&child);
        }
        let query = self
            .recipient
            .text()
            .trim()
            .trim_start_matches('@')
            .to_lowercase();
        let people = self.participants.borrow();
        let matches: Vec<_> = people
            .iter()
            .filter(|(id, name)| {
                !query.is_empty()
                    && (name.to_lowercase().contains(&query) || id.starts_with(&query))
            })
            .take(8)
            .collect();
        self.suggestions.set_visible(!matches.is_empty());
        for (id, name) in matches {
            let button = gtk::Button::with_label(&format!("{name} · {id}"));
            let id = id.clone();
            let ui = self.clone();
            button.connect_clicked(move |_| {
                ui.recipient.set_text(&id);
                ui.suggestions.set_visible(false);
            });
            self.suggestions.append(&button);
        }
    }

    fn set_source(&self, path: PathBuf) {
        if !path.is_file() {
            self.toast("Choose a regular file.");
            return;
        }
        self.source.replace(Some(path));
        self.refresh_source();
        self.send_button.set_sensitive(true);
    }

    fn refresh_source(&self) {
        if let Some(path) = self.source.borrow().as_ref() {
            self.source_title
                .set_label(&path.file_name().unwrap_or_default().to_string_lossy());
            self.source_title
                .set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            if let Ok(metadata) = path.metadata() {
                let chunk = self.chunk_size.value_as_int() as u64 * 1_000_000
                    - if self.encrypt.is_active() {
                        discord_parcel::crypto::OVERHEAD
                    } else {
                        0
                    };
                self.source_detail.set_label(&format!(
                    "{} / {} parts",
                    parcel::human_size(metadata.len()),
                    metadata.len().div_ceil(chunk).max(1),
                ));
            }
        }
    }

    fn choose_source(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose a file to send")
            .build();
        let ui = self.clone();
        dialog.open(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| match result {
                Ok(file) => {
                    if let Some(path) = file.path() {
                        ui.set_source(path);
                    }
                }
                Err(error) => ui.file_dialog_error(error),
            },
        );
    }

    fn set_manifest(&self, path: PathBuf) {
        self.receive_link.set_text("");
        self.receive_file
            .set_subtitle(&path.file_name().unwrap_or_default().to_string_lossy());
        if let Ok(manifest) = Manifest::read(&path) {
            self.decrypt.set_active(manifest.encryption.is_some());
        }
        self.parcel_file.replace(Some(path));
        self.stack.set_visible_child_name("receive");
    }

    fn choose_manifest(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Parcel transfer files"));
        filter.add_pattern("*.parcel.json");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Open transfer file")
            .filters(&filters)
            .build();
        let ui = self.clone();
        dialog.open(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| match result {
                Ok(file) => {
                    if let Some(path) = file.path() {
                        ui.set_manifest(path);
                    }
                }
                Err(error) => ui.file_dialog_error(error),
            },
        );
    }

    fn choose_destination(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Save restored file to")
            .initial_folder(&gio::File::for_path(self.destination.borrow().as_path()))
            .build();
        let ui = self.clone();
        dialog.select_folder(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| match result {
                Ok(file) => {
                    if let Some(path) = file.path() {
                        ui.destination_row.set_subtitle(&path.to_string_lossy());
                        ui.destination.replace(path);
                    }
                }
                Err(error) => ui.file_dialog_error(error),
            },
        );
    }

    fn file_dialog_error(&self, error: glib::Error) {
        if !error.matches(gtk::DialogError::Dismissed)
            && !error.matches(gtk::DialogError::Cancelled)
        {
            self.toast(&error.to_string());
        }
    }

    fn connection_dialog(self: &Rc<Self>) {
        let dialog = adw::PreferencesDialog::builder()
            .title("Discord Connection")
            .build();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder().build();
        let token = adw::PasswordEntryRow::builder().title("Bot token").build();
        token.set_text(&self.connection.borrow().token);
        let channel = adw::EntryRow::builder().title("Channel ID").build();
        channel.set_text(&self.connection.borrow().channel_id);
        group.add(&token);
        group.add(&channel);
        page.add(&group);
        let help = adw::PreferencesGroup::builder()
            .description(
                "Required permissions: View Channel, Send Messages, Attach Files, and Read Message History.\n
                To read another bot’s messages, also enable Message Content Intent.",
            )
            .build();
        let docs = gtk::LinkButton::with_label(
            "https://discord.com/developers/applications",
            "Open Discord Developer Portal",
        );
        help.add(&docs);
        let apply = gtk::Button::with_label("Save Connection");
        apply.add_css_class("suggested-action");
        apply.set_margin_top(12);
        help.add(&apply);
        page.add(&help);
        dialog.add(&page);
        let ui = self.clone();
        let d = dialog.clone();
        apply.connect_clicked(move |_| {
            let channel_id = channel.text().trim().to_owned();
            if !channel_id.is_empty() && !parcel::snowflake(&channel_id) {
                channel.add_css_class("error");
                return;
            }
            ui.connection.replace(Connection {
                token: token.text().trim().to_owned(),
                channel_id,
            });
            ui.channel_name.replace(None);
            ui.participants.borrow_mut().clear();
            ui.recipient.set_text("");
            ui.refresh_connection();
            ui.save_settings();
            d.close();
            ui.scan_connection();
        });
        dialog.present(Some(&self.window));
    }

    fn send(self: &Rc<Self>) {
        let Some(source) = self.source.borrow().clone() else {
            return;
        };
        let connection = self.connection.borrow().clone();
        if connection.token.is_empty() || connection.channel_id.is_empty() {
            self.connection_dialog();
            return;
        }
        let recipient_text = self.recipient.text();
        let recipient_text = recipient_text
            .trim()
            .trim_start_matches("<@")
            .trim_start_matches('!')
            .trim_end_matches('>');
        let recipient = if recipient_text.is_empty() {
            None
        } else if parcel::snowflake(recipient_text) {
            recipient_text.parse().ok()
        } else {
            self.toast("Choose a suggested person or enter their numeric Discord user ID.");
            return;
        };
        let password = self
            .encrypt
            .is_active()
            .then(|| self.send_password.text().to_string());
        if password.as_ref().is_some_and(String::is_empty) {
            self.toast("Enter an encryption passphrase first.");
            return;
        }
        let options = transfer::SendOptions {
            recipient,
            password,
        };
        let chunk_size = self.chunk_size.value_as_int() as u64 * 1_000_000;
        self.run_job(move |cancel, progress| {
            let result = transfer::upload(
                &source,
                &options,
                chunk_size,
                &connection,
                &parcel::data_dir(),
                cancel,
                progress,
            )?;
            Ok(Outcome {
                title: "Your parcel is ready".into(),
                detail: format!("{} parts uploaded. Share with someone else!", result.parts),
                file: Some(result.manifest_path),
                link: Some(result.message_link),
            })
        });
    }

    fn receive(self: &Rc<Self>) {
        let path = self.parcel_file.borrow().clone();
        let link = self.receive_link.text().trim().to_owned();
        if path.is_none() && link.is_empty() {
            self.toast("Open a transfer file first.");
            return;
        }
        let destination = self.destination.borrow().clone();
        let token = self.connection.borrow().token.clone();
        let password = self
            .decrypt
            .is_active()
            .then(|| self.receive_password.text().to_string());
        if password.as_ref().is_some_and(String::is_empty) {
            self.toast("Enter the sender’s decryption passphrase first.");
            return;
        }
        self.run_job(move |cancel, progress| {
            progress(Progress::new("Opening parcel", 0, 0));
            let manifest = if let Some(path) = &path {
                Manifest::read(path)?
            } else {
                transfer::manifest_from_link(&link, &token, cancel)?
            };
            let output = transfer::download(
                &manifest,
                password.as_deref(),
                &destination,
                &token,
                &parcel::data_dir(),
                cancel,
                progress,
            )?;
            Ok(Outcome {
                title: "Delivered, byte for byte".into(),
                detail: format!(
                    "{} / {}\nSHA-256 verified. Your restored file is identical to the sender’s original.",
                    manifest.filename,
                    parcel::human_size(manifest.size)
                ),
                file: Some(output),
                link: None,
            })
        });
    }

    fn show_outcome(self: &Rc<Self>, outcome: Outcome) {
        let Outcome {
            title,
            detail,
            file,
            link,
        } = outcome;

        let dialog = adw::AlertDialog::builder()
            .heading(&title)
            .body(&detail)
            .build();

        dialog.add_response("close", "Close");
        dialog.set_close_response("close");

        if link.is_some() {
            dialog.add_response("copy", "Copy Link");
            dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);
        }

        let is_manifest = file
            .as_ref()
            .is_some_and(|path| path.to_string_lossy().ends_with(".parcel.json"));

        if is_manifest {
            dialog.add_response("save", "Save Transfer File");
        }

        if file.is_some() {
            dialog.add_response("show", "Show Folder");
        }

        let ui = self.clone();
        dialog.connect_response(None, move |_, response| match response {
            "copy" => {
                if let Some(link) = link.as_ref() {
                    ui.window.clipboard().set_text(link);
                    ui.toast("Transfer link copied");
                }
            }
            "save" => {
                if let Some(source) = file.as_ref() {
                    ui.export_manifest(source.clone());
                }
            }
            "show" => {
                if let Some(parent) = file.as_ref().and_then(|path| path.parent()) {
                    let uri = gio::File::for_path(parent).uri();
                    if let Err(error) =
                        gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
                    {
                        ui.toast(&error.to_string());
                    }
                }
            }
            _ => {}
        });

        dialog.present(Some(&self.window));
    }

    fn show_error(&self, stopped: bool, error: &str) {
        let dialog = adw::AlertDialog::builder()
            .heading(if stopped {
                "Transfer stopped"
            } else {
                "Could not finish"
            })
            .body(if stopped {
                "Completed Discord parts are saved. Start the same transfer again to resume."
            } else {
                error
            })
            .build();

        dialog.add_response("close", "Close");
        dialog.set_close_response("close");
        dialog.present(Some(&self.window));
    }

    fn export_manifest(self: &Rc<Self>, source: PathBuf) {
        let dialog = gtk::FileDialog::builder()
            .title("Save transfer file")
            .initial_name("transfer.parcel.json")
            .build();
        let ui = self.clone();
        dialog.save(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| match result {
                Ok(file) => {
                    if let Some(path) = file.path() {
                        match parcel::read_bounded(&source, parcel::MAX_MANIFEST_SIZE)
                            .and_then(|bytes| parcel::atomic_write(&path, &bytes))
                        {
                            Ok(()) => ui.toast("Transfer file saved"),
                            Err(error) => ui.toast(&format!("Could not save: {error}")),
                        }
                    }
                }
                Err(error) => ui.file_dialog_error(error),
            },
        );
    }

    fn run_job<F>(self: &Rc<Self>, work: F)
    where
        F: FnOnce(&Cancel, &dyn Fn(Progress)) -> Result<Outcome> + Send + 'static,
    {
        if self.busy.replace(true) {
            return;
        }
        self.controls.set_sensitive(false);
        self.connection_button.set_sensitive(false);
        self.progress_box.set_visible(true);
        self.progress_bar.set_fraction(0.0);
        self.progress_title.set_label("Preparing…");
        self.progress_detail.set_label("");
        self.cancel_button.set_sensitive(true);
        self.spinner.start();
        let cancel = Cancel::default();
        self.cancel.replace(cancel.clone());
        let (sender, receiver) = async_channel::bounded(32);
        std::thread::spawn(move || {
            let progress = |progress| {
                let _ = sender.try_send(Event::Progress(progress));
            };
            let result = work(&cancel, &progress).map_err(|error| format!("{error:#}"));
            let _ = sender.send_blocking(Event::Finished(result));
        });
        let ui = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    Event::Progress(p) => {
                        ui.progress_title.set_label(&p.stage);
                        if p.total > 0 {
                            ui.progress_bar
                                .set_fraction((p.done as f64 / p.total as f64).clamp(0.0, 1.0));
                            ui.progress_detail.set_label(&format!(
                                "{} of {}",
                                parcel::human_size(p.done),
                                parcel::human_size(p.total)
                            ));
                        } else {
                            ui.progress_bar.pulse();
                            ui.progress_detail.set_label("Please wait…");
                        }
                    }
                    Event::Finished(result) => {
                        ui.busy.set(false);
                        ui.controls.set_sensitive(true);
                        ui.connection_button.set_sensitive(true);
                        ui.spinner.stop();
                        ui.progress_box.set_visible(false);

                        match result {
                            Ok(outcome) => ui.show_outcome(outcome),
                            Err(error) => {
                                let stopped = ui.cancel.borrow().check().is_err();
                                ui.show_error(stopped, &error);
                            }
                        }

                        break;
                    }
                }
            }
        });
    }
}

fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label
}

fn heading(title: &str, subtitle: &str) -> gtk::Box {
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

fn page() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, 18)
}

fn card() -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 12);
    box_.add_css_class("card");
    box_.add_css_class("transfer-card");
    box_
}
