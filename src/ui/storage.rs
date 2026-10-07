use adw::prelude::*;
use discord_parcel::{parcel, storage};
use gtk::glib;
use std::{cell::Cell, path::PathBuf, rc::Rc};

struct StorageView {
    dialog: glib::WeakRef<adw::PreferencesDialog>,
    root: PathBuf,
    rows: [adw::ActionRow; 5],
    status: gtk::Label,
    refresh: gtk::Button,
    clear: gtk::Button,
    busy: Cell<bool>,
    has_downloads: Cell<bool>,
}

pub(super) fn present(window: &adw::ApplicationWindow) {
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
        .description("File sizes only, excluding the app, runtime libraries, and restored files.")
        .build();
    let refresh = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh storage usage")
        .valign(gtk::Align::Center)
        .build();
    usage.set_header_suffix(Some(&refresh));
    let rows = [
        "Download cache",
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
        .title("Download cache cleanup")
        .description("Clear cached parts from stopped or interrupted downloads.")
        .build();
    let clear = gtk::Button::builder()
        .label("Clear Inactive Downloads…")
        .css_classes(["destructive-action"])
        .halign(gtk::Align::Start)
        .sensitive(false)
        .build();
    cleanup.add(&clear);
    let status = gtk::Label::builder()
        .label("Calculating storage usage…")
        .wrap(true)
        .xalign(0.0)
        .selectable(true)
        .margin_top(12)
        .build();
    cleanup.add(&status);
    page.add(&cleanup);
    dialog.add(&page);

    let view = Rc::new(StorageView {
        dialog: dialog.downgrade(),
        root,
        rows,
        status,
        refresh,
        clear,
        busy: Cell::new(false),
        has_downloads: Cell::new(false),
    });
    {
        let root = view.root.clone();
        let weak_dialog = dialog.downgrade();
        open.connect_clicked(move |_| {
            let result = std::fs::create_dir_all(&root).and_then(|()| open::that(&root));
            if let Err(error) = result
                && let Some(dialog) = weak_dialog.upgrade()
            {
                dialog.add_toast(adw::Toast::new(&format!(
                    "Could not open the data folder: {error}"
                )));
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
    view.update(false);
    dialog.connect_closed(move |_| {
        let _ = &view;
    });
    dialog.present(Some(window));
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
            .heading("Clear inactive download caches?")
            .body("This removes cached parts from downloads that are not currently running, including paused or interrupted downloads. Resuming them will require downloading those parts again.\n\nYour settings, upload checkpoints, saved transfer files, restored files, and Discord messages will not be deleted. Active caches and unrecognized files are skipped.")
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
        if self.busy.get() {
            return;
        }
        self.set_busy(true);
        self.status.set_label(if clear {
            "Clearing inactive download caches…"
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
            match receiver.recv().await {
                Ok((cleanup, usage)) => {
                    let mut messages = Vec::new();
                    if let Some(cleanup) = cleanup {
                        messages.push(match cleanup {
                            Ok(report) => format!(
                                "Removed {} from {} download cache(s). Skipped {} active, empty, or unrecognized entry/entries. Failed to clear {} entry/entries.",
                                parcel::human_size(report.removed_bytes),
                                report.removed_caches,
                                report.skipped_caches,
                                report.failed_caches,
                            ),
                            Err(error) => format!(
                                "Cleanup could not finish: {error}\nSome caches may already have been cleared."
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
                            if messages.is_empty() {
                                messages.push("Usage updated.".into());
                            }
                        }
                        Err(error) => {
                            for row in &view.rows {
                                row.set_subtitle("Unavailable");
                            }
                            view.has_downloads.set(false);
                            messages.push(format!("Could not measure storage: {error}"));
                        }
                    }
                    view.status.set_label(&messages.join("\n\n"));
                }
                Err(_) => {
                    view.status.set_label("The storage operation stopped unexpectedly. Refresh to check usage before trying again.");
                }
            }
            view.set_busy(false);
        });
    }
}
