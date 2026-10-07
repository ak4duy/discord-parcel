use super::Ui;
use adw::prelude::*;
use anyhow::Result;
use discord_parcel::{
    discord::Discord,
    parcel::{self, Cancel},
    transfer::Connection,
};
use gtk::glib;
use std::rc::Rc;

impl Ui {
    pub(super) fn refresh_connection(&self) {
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

    pub(super) fn scan_connection(self: &Rc<Self>) {
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
                    ui.connection_row.set_subtitle("Connection failed");
                    ui.toast(&error);
                }
            }
        });
    }

    pub(super) fn refresh_suggestions(self: &Rc<Self>) {
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

    pub(super) fn connection_dialog(self: &Rc<Self>) {
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
                "Required permissions: View Channel, Send Messages, Attach Files, and Read Message History.\nTo read another bot’s messages, also enable Message Content Intent.",
            )
            .build();
        let docs = gtk::LinkButton::with_label(
            "https://discord.com/developers/applications",
            "Open Discord Developer Portal",
        );
        let ui = self.clone();
        docs.connect_activate_link(move |button| {
            if let Err(error) = open::that(button.uri().as_str()) {
                ui.toast(&format!(
                    "Could not open the Discord Developer Portal: {error}"
                ));
            }
            glib::Propagation::Stop
        });
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
}
