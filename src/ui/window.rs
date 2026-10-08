use adw::prelude::{ActionRowExt, AdwApplicationWindowExt, AdwDialogExt};
use discord_parcel::{parcel::Cancel, transfer::Connection};
use gtk::{
    gio::{
        self,
        prelude::{ActionMapExt, FileExt},
    },
    glib::{self, prelude::StaticType},
    prelude::{BoxExt, ButtonExt, EditableExt, GtkApplicationExt, GtkWindowExt, WidgetExt},
};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use super::{
    Ui,
    pages::{ReceivePage, SendPage, TransferProgress},
    settings::Settings,
    storage,
};

pub fn present(app: &adw::Application, initial: Option<PathBuf>) {
    if initial.is_none()
        && let Some(window) = app.active_window()
    {
        window.present();
        return;
    }
    let startup = app.windows().is_empty();
    let css = gtk::CssProvider::new();
    css.load_from_string(include_str!("../../data/style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let settings = Settings::load();
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
    menu.append(Some("Storage"), Some("win.storage"));
    menu.append(Some("Check for Updates"), Some("win.check-updates"));
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
    let send = SendPage::new(settings.chunk_mb);
    stack.add_titled_with_icon(&send.page, Some("send"), "Send", "document-send-symbolic");
    let receive = ReceivePage::new();
    stack.add_titled_with_icon(
        &receive.page,
        Some("receive"),
        "Receive",
        "folder-download-symbolic",
    );
    let progress = TransferProgress::new();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 20);
    content.set_margin_top(12);
    content.set_margin_bottom(28);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(&controls);
    content.append(&progress.progress_box);
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
        destination: RefCell::new(receive.destination),
        connection: RefCell::new(Connection {
            token: std::env::var("DISCORD_BOT_TOKEN").unwrap_or(settings.token),
            channel_id: settings.channel_id,
        }),
        chunk_size: send.chunk_size,
        channel_name: RefCell::new(None),
        recipient: send.recipient,
        suggestions: send.suggestions,
        participants: RefCell::new(Vec::new()),
        encrypt: send.encrypt,
        send_password: send.send_password,
        decrypt: receive.decrypt,
        receive_password: receive.receive_password,
        source_title: send.source_title,
        source_detail: send.source_detail,
        receive_file: receive.receive_file,
        receive_link: receive.receive_link,
        destination_row: receive.destination_row,
        connection_row: send.connection_row,
        send_button: send.send_button,

        stack,
        progress_box: progress.progress_box,
        progress_title: progress.progress_title,
        progress_detail: progress.progress_detail,
        progress_bar: progress.progress_bar,
        spinner: progress.spinner,
        cancel_button: progress.cancel_button,
        busy: Cell::new(false),
        cancel: RefCell::new(Cancel::default()),
    });
    ui.refresh_connection();
    ui.scan_connection();
    {
        let copy = ui.clone();
        send.scan.connect_clicked(move |_| copy.scan_connection());
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
        send.pick.connect_clicked(move |_| ui.choose_source());
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
    send.pick.add_controller(drop);
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
        receive
            .browse_channel
            .connect_activated(move |_| copy.browse_channel_transfers());
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
        receive
            .receive_button
            .connect_clicked(move |_| copy.receive());
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
    let check_updates = install_actions(&ui);
    if let Some(path) = initial {
        if path.to_string_lossy().ends_with(".parcel.json") {
            ui.set_manifest(path);
        } else {
            ui.set_source(path);
        }
    }
    ui.window.present();
    if startup {
        ui.check_updates(&check_updates, true);
    }
}

fn install_actions(ui: &Rc<Ui>) -> gio::SimpleAction {
    let storage = gio::SimpleAction::new("storage", None);
    {
        let weak = Rc::downgrade(ui);
        storage.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                storage::present(&ui);
            }
        });
    }
    ui.window.add_action(&storage);

    let check_updates = gio::SimpleAction::new("check-updates", None);
    {
        let copy = ui.clone();
        check_updates.connect_activate(move |action, _| copy.check_updates(action, false));
    }
    ui.window.add_action(&check_updates);

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
    check_updates
}
