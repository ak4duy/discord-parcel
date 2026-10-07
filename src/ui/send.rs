use super::{Outcome, Ui};
use adw::prelude::*;
use discord_parcel::{parcel, transfer};
use gtk::gio;
use std::{path::PathBuf, rc::Rc};

impl Ui {
    pub(super) fn set_source(&self, path: PathBuf) {
        if !path.is_file() {
            self.toast("Choose a regular file.");
            return;
        }
        self.source.replace(Some(path));
        self.refresh_source();
        self.send_button.set_sensitive(true);
    }

    pub(super) fn refresh_source(&self) {
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

    pub(super) fn choose_source(self: &Rc<Self>) {
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

    pub(super) fn send(self: &Rc<Self>) {
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
}
