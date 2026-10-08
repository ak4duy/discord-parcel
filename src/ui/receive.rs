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
        self.receive_file
            .set_subtitle(&path.file_name().unwrap_or_default().to_string_lossy());
        if let Ok(manifest) = Manifest::read(&path) {
            self.decrypt.set_active(manifest.encryption.is_some());
        }
        self.parcel_file.replace(Some(path));
        self.stack.set_visible_child_name("receive");
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
        let password = self
            .decrypt
            .is_active()
            .then(|| self.receive_password.text().to_string());
        let lookup_token = token.clone();
        self.run_job_with_completion(
            move |cancel, progress| {
                progress(Progress::new("Opening parcel", 0, 0));
                let manifest = if let Some(path) = &path {
                    Manifest::read(path)?
                } else {
                    transfer::manifest_from_link(&link, &lookup_token, cancel)?
                };
                cancel.check()?;
                Ok(manifest)
            },
            move |ui, manifest| {
                ui.decrypt.set_active(manifest.encryption.is_some());
                if manifest.encryption.is_some() && password.as_ref().is_none_or(String::is_empty) {
                    ui.prompt_passphrase(manifest, destination, token);
                } else {
                    ui.download_manifest(manifest, destination, token, password);
                }
            },
        );
    }

    pub(super) fn prompt_passphrase(
        self: &Rc<Self>,
        manifest: Manifest,
        destination: PathBuf,
        token: String,
    ) {
        let password = adw::PasswordEntryRow::builder()
            .title("Decryption passphrase")
            .build();
        let group = adw::PreferencesGroup::new();
        group.add(&password);
        let dialog = adw::AlertDialog::builder()
            .heading("This parcel is encrypted")
            .body(format!(
                "Enter the sender’s passphrase to restore {}.",
                manifest.filename
            ))
            .extra_child(&group)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("decrypt", "Decrypt and Download");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("decrypt"));
        dialog.set_response_appearance("decrypt", adw::ResponseAppearance::Suggested);
        dialog.set_response_enabled("decrypt", false);
        let weak_dialog = dialog.downgrade();
        password.connect_changed(move |entry| {
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.set_response_enabled("decrypt", !entry.text().is_empty());
            }
        });
        let ui = self.clone();
        dialog.connect_response(Some("decrypt"), move |_, _| {
            let passphrase = password.text().to_string();
            password.set_text("");
            if !passphrase.is_empty() {
                ui.download_manifest(
                    manifest.clone(),
                    destination.clone(),
                    token.clone(),
                    Some(passphrase),
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
    ) {
        let mode = if self.low_disk.is_active() {
            if manifest.encryption.is_some() {
                self.toast("Low-disk mode leaves decrypted partial content on disk, even after cancellation.");
            }
            transfer::ReceiveMode::LowDisk
        } else {
            transfer::ReceiveMode::Standard
        };
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
