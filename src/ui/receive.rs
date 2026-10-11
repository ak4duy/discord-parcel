use super::{Outcome, Ui};
use adw::prelude::*;
use discord_parcel::{
    parcel::{self, Manifest, Progress},
    transfer,
};
use gtk::gio;
use std::{path::PathBuf, rc::Rc};

impl Ui {
    pub(super) fn set_manifest(&self, path: PathBuf) {
        self.receive_link.set_text("");
        self.parcel_file.replace(None);
        self.receive_password.set_text("");
        self.stack.set_visible_child_name("receive");
        let result = Manifest::read(&path).and_then(|manifest| {
            validate_receive_manifest(&manifest)?;
            Ok(manifest)
        });
        match result {
            Ok(manifest) => {
                self.receive_file.set_subtitle(&format!(
                    "{}  {}  {} parts",
                    manifest.filename,
                    parcel::human_size(manifest.size),
                    manifest.parts.len(),
                ));
                self.decrypt.set_active(manifest.encryption.is_some());
                self.parcel_file.replace(Some(path));
            }
            Err(error) => {
                self.receive_file.set_subtitle("Choose a .parcel.json file");
                self.decrypt.set_active(false);
                let dialog = adw::AlertDialog::builder()
                    .heading("Could not open transfer file")
                    .body(format!("{error:#}"))
                    .build();
                dialog.add_response("close", "Close");
                dialog.set_close_response("close");
                dialog.present(Some(&self.window));
            }
        }
    }

    pub(super) fn choose_manifest(self: &Rc<Self>) {
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

    pub(super) fn choose_destination(self: &Rc<Self>) {
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

    pub(super) fn receive(self: &Rc<Self>) {
        let path = self.parcel_file.borrow().clone();
        let link = self.receive_link.text().trim().to_owned();
        if path.is_none() && link.is_empty() {
            self.toast("Open a transfer file first.");
            return;
        }
        let destination = self.destination.borrow().clone();
        let token = self.connection.borrow().token.clone();
        let mode = if self.low_disk.is_active() {
            transfer::ReceiveMode::LowDisk
        } else {
            transfer::ReceiveMode::Standard
        };
        let lookup_token = token.clone();
        self.run_job_with_completion(
            move |cancel, progress| {
                progress(Progress::new("Opening parcel", 0, 0));
                let manifest = if let Some(path) = &path {
                    Manifest::read(path)?
                } else {
                    transfer::manifest_from_link(&link, &lookup_token, cancel)?
                };
                validate_receive_manifest(&manifest)?;
                cancel.check()?;
                Ok(manifest)
            },
            move |ui, manifest| {
                ui.decrypt.set_active(manifest.encryption.is_some());
                ui.preview_download(manifest, destination, token, mode);
            },
        );
    }

    fn preview_download(
        self: &Rc<Self>,
        manifest: Manifest,
        destination: PathBuf,
        token: String,
        mode: transfer::ReceiveMode,
    ) {
        let encrypted = manifest.encryption.is_some();
        let low_disk = mode == transfer::ReceiveMode::LowDisk;
        let output = destination.join(parcel::output_filename(&manifest.filename));
        let disk_space = if low_disk {
            format!(
                "About {} in the destination folder",
                parcel::human_size(manifest.size)
            )
        } else {
            let cache_size = manifest.size
                + if encrypted {
                    manifest.parts.len() as u64 * discord_parcel::crypto::OVERHEAD
                } else {
                    0
                };
            format!(
                "About {} in the destination folder, plus {} for cached parts",
                parcel::human_size(manifest.size),
                parcel::human_size(cache_size),
            )
        };
        let details = adw::PreferencesGroup::builder().build();
        for (title, subtitle) in [
            ("File", manifest.filename.clone()),
            ("Size", parcel::human_size(manifest.size)),
            ("Parts", manifest.parts.len().to_string()),
            (
                "Encryption",
                if encrypted {
                    "Encrypted"
                } else {
                    "Not encrypted"
                }
                .into(),
            ),
            ("Save to", output.to_string_lossy().into_owned()),
            (
                "Download mode",
                if low_disk { "Low-disk" } else { "Standard" }.into(),
            ),
            ("Estimated disk space", disk_space),
        ] {
            details.add(
                &adw::ActionRow::builder()
                    .title(title)
                    .subtitle(subtitle)
                    .use_markup(false)
                    .build(),
            );
        }
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&details);
        let password = adw::PasswordEntryRow::builder()
            .title("Decryption passphrase")
            .build();
        if encrypted {
            password.set_text(&self.receive_password.text());
            let group = adw::PreferencesGroup::new();
            if low_disk {
                group.set_description(Some(
                    "Low-disk mode leaves decrypted partial content on disk, even after cancellation.",
                ));
            }
            group.add(&password);
            content.append(&group);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(550)
            .propagate_natural_height(true)
            .max_content_height(650)
            .child(&content)
            .build();
        let dialog = adw::AlertDialog::builder()
            .heading("Preview download")
            .follows_content_size(true)
            .presentation_mode(adw::DialogPresentationMode::Floating)
            .extra_child(&scroll)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("download", "Download and Restore");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("download"));
        dialog.set_response_appearance("download", adw::ResponseAppearance::Suggested);
        dialog.set_response_enabled("download", !encrypted || !password.text().is_empty());
        let weak_dialog = dialog.downgrade();
        password.connect_changed(move |entry| {
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.set_response_enabled("download", !encrypted || !entry.text().is_empty());
            }
        });
        let ui = self.clone();
        dialog.connect_response(Some("download"), move |_, _| {
            let passphrase = encrypted.then(|| password.text().to_string());
            password.set_text("");
            if passphrase.as_ref().is_none_or(|value| !value.is_empty()) {
                ui.download_manifest(
                    manifest.clone(),
                    destination.clone(),
                    token.clone(),
                    passphrase,
                    mode,
                );
            }
        });
        dialog.present(Some(&self.window));
    }

    pub(super) fn download_manifest(
        self: &Rc<Self>,
        manifest: Manifest,
        destination: PathBuf,
        token: String,
        password: Option<String>,
        mode: transfer::ReceiveMode,
    ) {
        self.run_job(move |cancel, progress| {
            let output = transfer::download_with_options(
                &manifest,
                &transfer::ReceiveOptions { password: password.as_deref(), mode },
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
}

fn validate_receive_manifest(manifest: &Manifest) -> anyhow::Result<()> {
    anyhow::ensure!(
        manifest.parts.iter().all(|part| part.remote.is_some()),
        "This parcel is missing Discord attachment information for one or more parts."
    );
    Ok(())
}
