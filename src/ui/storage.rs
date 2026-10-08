use super::Ui;
use adw::prelude::*;
use discord_parcel::{parcel, storage};
use gtk::glib;
use std::{
    cell::Cell,
    path::PathBuf,
    rc::{Rc, Weak},
};

struct StorageView {
    ui: Weak<Ui>,
    dialog: glib::WeakRef<adw::PreferencesDialog>,
    root: PathBuf,
    rows: [adw::ActionRow; 5],

    refresh: gtk::Button,
    clear: gtk::Button,
    busy: Cell<bool>,
    closed: Cell<bool>,
    has_downloads: Cell<bool>,
}

pub(super) fn present(ui: &Rc<Ui>) {
    let root = parcel::data_dir();
    let dialog = adw::PreferencesDialog::builder()
        .title("Storage")
        .content_width(600)
        .content_height(650)
        .build();
    let page = adw::PreferencesPage::new();
    let location = adw::PreferencesGroup::builder()
        .title("Data location")
        .description("Settings and transfer data are stored here.")
        .build();
    let path = adw::ActionRow::builder()
        .title(if parcel::is_portable() {
            "Portable besides application folder"
        } else {
            "System userdata folder"
        })
        .subtitle(root.to_string_lossy())
        .use_markup(false)
        .build();
    let open = gtk::Button::builder()
        .label("Open Data Folder")
        .valign(gtk::Align::Center)
        .build();
    path.add_suffix(&open);
    location.add(&path);
    page.add(&location);

    let usage = adw::PreferencesGroup::builder()
        .title("Managed data")
        .description("File sizes only, excluding the app, runtime libraries, restored files,..")
        .build();
    let refresh = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh storage usage")
        .valign(gtk::Align::Center)
        .build();
    usage.set_header_suffix(Some(&refresh));
    let rows = [
        "Downloads",
        "Upload checkpoints",
        "Saved transfer files",
        "Settings",
        "Total managed data",
    ]
    .map(|title| {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle("Calculating…")
            .use_markup(false)
            .build();
        usage.add(&row);
        row
    });
    page.add(&usage);

    let cleanup = adw::PreferencesGroup::builder()
        .title("Download cleanup")
        .description("Clear stopped downloads, including Standard cached parts and Low-disk partial files in your destination folders.")
        .build();
    let clear = gtk::Button::builder()
        .label("Clear Inactive Downloads…")
        .css_classes(["destructive-action"])
        .halign(gtk::Align::Start)
        .sensitive(false)
        .build();
    cleanup.add(&clear);

    page.add(&cleanup);
    dialog.add(&page);

    let view = Rc::new(StorageView {
        ui: Rc::downgrade(ui),
        dialog: dialog.downgrade(),
        root,
        rows,

        refresh,
        clear,
        busy: Cell::new(false),
        closed: Cell::new(false),
        has_downloads: Cell::new(false),
    });
    {
        let root = view.root.clone();
        let weak_ui = Rc::downgrade(ui);
        open.connect_clicked(move |_| {
            let result = std::fs::create_dir_all(&root).and_then(|()| open::that(&root));
            if let Err(error) = result
                && let Some(ui) = weak_ui.upgrade()
            {
                ui.toast(&format!("Could not open the data folder: {error}"));
            }
        });
    }
    {
        let weak = Rc::downgrade(&view);
        view.refresh.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.update(false);
            }
        });
    }
    {
        let weak = Rc::downgrade(&view);
        view.clear.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.confirm_cleanup();
            }
        });
    }
    let closing_view = view.clone();
    dialog.connect_closed(move |_| {
        closing_view.closed.set(true);
    });
    dialog.present(Some(&ui.window));
    view.update(false);
}

impl StorageView {
    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.refresh.set_sensitive(!busy);
        self.clear.set_sensitive(!busy && self.has_downloads.get());
    }

    fn confirm_cleanup(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let Some(parent) = self.dialog.upgrade() else {
            return;
        };
        self.set_busy(true);
        let confirmation = adw::AlertDialog::builder()
            .heading("Clear inactive downloads?")
            .body("This deletes cached parts and resume data for stopped or interrupted downloads in both modes.")
            .build();
        confirmation.add_response("cancel", "Cancel");
        confirmation.add_response("clear", "Clear Inactive Downloads");
        confirmation.set_close_response("cancel");
        confirmation.set_default_response(Some("cancel"));
        confirmation.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        let view = self.clone();
        confirmation.connect_response(None, move |_, response| {
            view.set_busy(false);
            if response == "clear" {
                view.update(true);
            }
        });
        confirmation.present(Some(&parent));
    }

    fn update(self: &Rc<Self>, clear: bool) {
        if self.busy.get() || self.closed.get() {
            return;
        }
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        self.set_busy(true);
        ui.toast(if clear {
            "Clearing inactive downloads…"
        } else {
            "Calculating storage usage…"
        });
        let root = self.root.clone();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let cleanup = clear.then(|| {
                storage::clear_inactive_downloads(&root).map_err(|error| format!("{error:#}"))
            });
            let usage = storage::usage(&root).map_err(|error| format!("{error:#}"));
            let _ = sender.send_blocking((cleanup, usage));
        });
        let view = self.clone();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await;
            if view.closed.get() {
                return;
            }
            let Some(ui) = view.ui.upgrade() else {
                return;
            };
            match result {
                Ok((cleanup, usage)) => {
                    if let Some(cleanup) = cleanup {
                        ui.toast(&match cleanup {
                            Ok(report) => format!(
                                "Removed {} from {} download entries. Skipped {} active, empty, or unrecognized entries. Failed to clear {} entries.",
                                parcel::human_size(report.removed_bytes),
                                report.removed_caches,
                                report.skipped_caches,
                                report.failed_caches,
                            ),
                            Err(error) => format!(
                                "Cleanup could not finish: {error}. Some files may already have been deleted."
                            ),
                        });
                    }
                    match usage {
                        Ok(usage) => {
                            let sizes = [
                                usage.downloads_bytes,
                                usage.uploads_bytes,
                                usage.sent_bytes,
                                usage.settings_bytes,
                            ];
                            let total = sizes.iter().copied().fold(0u64, u64::saturating_add);
                            for (row, bytes) in
                                view.rows.iter().zip(sizes.into_iter().chain([total]))
                            {
                                row.set_subtitle(&parcel::human_size(bytes));
                            }
                            view.has_downloads.set(usage.downloads_bytes > 0);
                            if !clear {
                                ui.toast("Usage updated.");
                            }
                        }
                        Err(error) => {
                            for row in &view.rows {
                                row.set_subtitle("Unavailable");
                            }
                            view.has_downloads.set(false);
                            ui.toast(&format!("Could not measure storage: {error}"));
                        }
                    }
                }
                Err(_) => {
                    ui.toast("The storage operation stopped unexpectedly. Refresh to check usage before trying again.");
                }
            }
            view.set_busy(false);
        });
    }
}
