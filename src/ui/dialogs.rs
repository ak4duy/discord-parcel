use super::{Outcome, Ui};
use adw::prelude::*;
use discord_parcel::parcel;
use gtk::{gio, glib};
use std::{path::PathBuf, rc::Rc};

impl Ui {
    pub(super) fn file_dialog_error(&self, error: glib::Error) {
        if !error.matches(gtk::DialogError::Dismissed)
            && !error.matches(gtk::DialogError::Cancelled)
        {
            self.toast(&error.to_string());
        }
    }

    pub(super) fn show_outcome(self: &Rc<Self>, outcome: Outcome) {
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
                if let Some(parent) = file.as_ref().and_then(|path| path.parent())
                    && let Err(error) = open::that(parent)
                {
                    ui.toast(&format!("Could not open the folder: {error}"));
                }
            }
            _ => {}
        });

        dialog.present(Some(&self.window));
    }

    pub(super) fn show_error(&self, stopped: bool, error: &str) {
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

    pub(super) fn export_manifest(self: &Rc<Self>, source: PathBuf) {
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
                        };
                    }
                }
                Err(error) => ui.file_dialog_error(error),
            },
        );
    }
}
