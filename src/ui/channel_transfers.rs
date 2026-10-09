use super::Ui;
use adw::prelude::*;
use discord_parcel::{
    discord::{ChannelTransfer, Discord},
    parcel::{self, Cancel},
    transfer::Connection,
};
use gtk::glib;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::{Rc, Weak},
};

struct ChannelBrowser {
    ui: Weak<Ui>,
    dialog: glib::WeakRef<adw::PreferencesDialog>,
    connection: Connection,
    transfers: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
    seen_transfers: RefCell<HashSet<String>>,

    refresh: gtk::Button,
    older: gtk::Button,
    stop: gtk::Button,
    before: RefCell<Option<String>>,
    scanned: Cell<usize>,
    skipped: Cell<usize>,
    busy: Cell<bool>,
    closed: Cell<bool>,
    cancel: RefCell<Cancel>,
    loading: RefCell<Option<adw::Toast>>,
}

impl Ui {
    pub(super) fn browse_channel_transfers(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let connection = self.connection.borrow().clone();
        if connection.token.trim().is_empty() || connection.channel_id.trim().is_empty() {
            self.toast("Connect a bot and choose a channel to browse transfers.");
            self.connection_dialog();
            return;
        }
        let dialog = adw::PreferencesDialog::builder()
            .title("Channel Transfers")
            .content_width(620)
            .content_height(650)
            .build();
        let page = adw::PreferencesPage::new();
        let channel = self
            .channel_name
            .borrow()
            .as_ref()
            .map(|name| format!("#{name}"))
            .unwrap_or_else(|| format!("Channel {}", connection.channel_id));
        let controls = adw::PreferencesGroup::builder().title(&channel).build();
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_margin_top(12);
        let refresh = gtk::Button::with_label("Refresh");
        let older = gtk::Button::with_label("Load Older");
        let stop = gtk::Button::with_label("Stop");
        buttons.append(&refresh);
        buttons.append(&older);
        buttons.append(&stop);
        controls.add(&buttons);

        page.add(&controls);
        let transfers = adw::PreferencesGroup::builder()
            .title("Available transfers")
            .build();
        page.add(&transfers);
        let help = adw::PreferencesGroup::builder()
            .description("The bot needs View Channel and Read Message History. To see transfers sent by another bot, please enable Message Content Intent.")
            .build();
        page.add(&help);
        dialog.add(&page);
        let browser = Rc::new(ChannelBrowser {
            ui: Rc::downgrade(self),
            dialog: dialog.downgrade(),
            connection,
            transfers,
            rows: RefCell::new(Vec::new()),
            seen_transfers: RefCell::new(HashSet::new()),

            refresh,
            older,
            stop,
            before: RefCell::new(None),
            scanned: Cell::new(0),
            skipped: Cell::new(0),
            busy: Cell::new(false),
            closed: Cell::new(false),
            cancel: RefCell::new(Cancel::default()),
            loading: RefCell::new(None),
        });
        {
            let weak = Rc::downgrade(&browser);
            browser.refresh.connect_clicked(move |_| {
                if let Some(browser) = weak.upgrade() {
                    browser.load(true);
                }
            });
        }
        {
            let weak = Rc::downgrade(&browser);
            browser.older.connect_clicked(move |_| {
                if let Some(browser) = weak.upgrade() {
                    browser.load(false);
                }
            });
        }
        {
            let weak = Rc::downgrade(&browser);
            browser.stop.connect_clicked(move |_| {
                if let Some(browser) = weak.upgrade() {
                    browser.cancel.borrow().cancel();
                    browser.stop.set_sensitive(false);
                    if let Some(loading) = browser.loading.borrow().as_ref() {
                        loading.set_title(
                            "Stopping… An ongoing manifest download may need to finish first.",
                        );
                    }
                }
            });
        }
        let closing_browser = browser.clone();
        dialog.connect_closed(move |_| {
            closing_browser.closed.set(true);
            closing_browser.cancel.borrow().cancel();
            if let Some(loading) = closing_browser.loading.borrow_mut().take() {
                loading.dismiss();
            }
        });
        dialog.present(Some(&self.window));
        browser.load(true);
    }
}

impl ChannelBrowser {
    fn connection_matches(&self, ui: &Ui) -> bool {
        let current = ui.connection.borrow();
        current.channel_id == self.connection.channel_id && current.token == self.connection.token
    }

    fn connection_changed(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.toast("Connection changed. Close this browser and open it again.");
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.refresh.set_sensitive(!busy);
        self.older
            .set_sensitive(!busy && self.before.borrow().is_some());
        self.stop.set_visible(busy);
        self.stop.set_sensitive(busy);
        self.transfers.set_sensitive(!busy);
    }

    fn load(self: &Rc<Self>, reset: bool) {
        if self.busy.get() || self.closed.get() {
            return;
        }
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if !self.connection_matches(&ui) {
            self.connection_changed();
            return;
        }
        let before = if reset {
            None
        } else {
            let Some(before) = self.before.borrow().clone() else {
                return;
            };
            Some(before)
        };
        self.set_busy(true);
        let loading = ui.toast("Scanning channel messages and reading transfer manifests…");
        loading.set_timeout(0);
        self.loading.replace(Some(loading));
        let cancel = Cancel::default();
        self.cancel.replace(cancel.clone());
        let connection = self.connection.clone();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = Discord::new(&connection.token)
                .and_then(|discord| {
                    discord.transfers(&connection.channel_id, before.as_deref(), &cancel)
                })
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send_blocking(result);
        });
        let browser = self.clone();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await.unwrap_or_else(|_| {
                Err("The channel scan stopped unexpectedly. Try again.".into())
            });
            if let Some(loading) = browser.loading.borrow_mut().take() {
                loading.dismiss();
            }
            if browser.closed.get() {
                return;
            }
            let Some(ui) = browser.ui.upgrade() else {
                return;
            };
            if !browser.connection_matches(&ui) {
                browser.set_busy(false);
                browser.connection_changed();
                return;
            }
            if browser.cancel.borrow().check().is_err() {
                ui.toast("Scan stopped. Previous results are kept.");
            } else {
                match result {
                    Ok(page) => {
                        if reset {
                            for row in browser.rows.borrow_mut().drain(..) {
                                browser.transfers.remove(&row);
                            }
                            browser.seen_transfers.borrow_mut().clear();
                            browser.scanned.set(0);
                            browser.skipped.set(0);
                        }
                        browser
                            .scanned
                            .set(browser.scanned.get().saturating_add(page.scanned));
                        browser
                            .skipped
                            .set(browser.skipped.get().saturating_add(page.skipped));
                        browser.before.replace(page.before);
                        for transfer in page.transfers {
                            browser.add_transfer(transfer);
                        }
                        let summary = format!(
                            "Found {} transfers in {} messages. Skipped {} manifests.",
                            browser.rows.borrow().len(),
                            browser.scanned.get(),
                            browser.skipped.get(),
                        );
                        ui.toast(&summary);
                    }
                    Err(error) => {
                        ui.toast(&format!("Could not scan the channel: {error}"));
                    }
                }
            }
            browser.set_busy(false);
        });
    }

    fn add_transfer(self: &Rc<Self>, transfer: ChannelTransfer) {
        if !self
            .seen_transfers
            .borrow_mut()
            .insert(transfer.transfer_key.clone())
        {
            return;
        }
        let row = adw::ActionRow::builder()
            .title(&transfer.filename)
            .subtitle(format!(
                "{} — {}",
                parcel::human_size(transfer.size),
                if transfer.encrypted {
                    "Encrypted"
                } else {
                    "Not encrypted"
                },
            ))
            .use_markup(false)
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = Rc::downgrade(self);
        row.connect_activated(move |_| {
            let Some(browser) = weak.upgrade() else {
                return;
            };
            let Some(ui) = browser.ui.upgrade() else {
                return;
            };
            if browser.busy.get() || browser.closed.get() || ui.busy.get() {
                return;
            }
            if !browser.connection_matches(&ui) {
                browser.connection_changed();
                return;
            }
            ui.parcel_file.replace(None);
            ui.receive_link.set_text(&transfer.message_link);
            ui.decrypt.set_active(transfer.encrypted);
            ui.receive_password.set_text("");
            ui.stack.set_visible_child_name("receive");
            if let Some(dialog) = browser.dialog.upgrade() {
                dialog.close();
            }
            ui.toast("Transfer selected. Choose a destination, then Download and Restore.");
        });
        self.transfers.add(&row);
        self.rows.borrow_mut().push(row);
    }
}
