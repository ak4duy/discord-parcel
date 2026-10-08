mod channel_transfers;
mod connection;
mod dialogs;
mod jobs;
mod pages;
mod receive;
mod send;
mod settings;
mod storage;
mod updates;
mod widgets;
mod window;

pub use window::present;

use adw::prelude::*;
use discord_parcel::{parcel::Cancel, transfer::Connection};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
};

struct Outcome {
    title: String,
    detail: String,
    file: Option<PathBuf>,
    link: Option<String>,
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
    low_disk: adw::SwitchRow,
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

impl Ui {
    fn toast(&self, text: &str) -> adw::Toast {
        let toast = adw::Toast::new(text);
        if let Some(dialog) = self
            .window
            .visible_dialog()
            .and_then(|dialog| dialog.downcast::<adw::PreferencesDialog>().ok())
        {
            dialog.add_toast(toast.clone());
        } else {
            self.toast.add_toast(toast.clone());
        }
        toast
    }
}
