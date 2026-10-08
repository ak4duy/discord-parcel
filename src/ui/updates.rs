use super::Ui;
use adw::prelude::*;
use discord_parcel::updates;
use gtk::{gio, glib};
use std::rc::Rc;

impl Ui {
    pub(super) fn check_updates(self: &Rc<Self>, action: &gio::SimpleAction, automatic: bool) {
        action.set_enabled(false);
        let checking = self.toast("Checking for updates…");
        checking.set_timeout(0);

        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = updates::check().map_err(|error| format!("{error:#}"));
            let _ = sender.send_blocking(result);
        });

        let ui = self.clone();
        let action = action.clone();
        glib::spawn_future_local(async move {
            let result = receiver.recv().await.unwrap_or_else(|_| {
                Err("The update check stopped unexpectedly. Try again.".into())
            });
            checking.dismiss();
            action.set_enabled(true);

            if automatic {
                let message = match result {
                    Ok(Some(release)) if release.newer => format!(
                        "Discord Parcel {} is available. Open Check for Updates for details.",
                        release.version
                    ),
                    Ok(Some(_)) => "You’re up to date".into(),
                    Ok(None) => "No public stable release is available".into(),
                    Err(error) => format!("Could not check for updates: {error}"),
                };
                ui.toast(&message);
                return;
            }

            let current = env!("CARGO_PKG_VERSION");
            let (title, detail, url, available) = match result {
                Ok(Some(release)) if release.newer => (
                    "Update available",
                    format!(
                        "Discord Parcel {} is available. You’re running {}.\n\nOpen the release page for downloads and release notes.",
                        release.version, current
                    ),
                    release.url,
                    true,
                ),
                Ok(Some(release)) => (
                    "You’re up to date",
                    format!(
                        "You’re running Discord Parcel {}. The latest published release is {}.",
                        current, release.version
                    ),
                    release.url,
                    false,
                ),
                Ok(None) => (
                    "No release available",
                    format!(
                        "You’re running Discord Parcel {}. No public stable release was found on GitHub.",
                        current
                    ),
                    updates::RELEASES_URL.into(),
                    false,
                ),
                Err(error) => (
                    "Could not check for updates",
                    error,
                    updates::RELEASES_URL.into(),
                    false,
                ),
            };

            let dialog = adw::AlertDialog::builder()
                .heading(title)
                .body(&detail)
                .build();
            dialog.add_response("close", "Close");
            dialog.add_response(
                "release",
                if available {
                    "View Update"
                } else {
                    "Open Releases"
                },
            );
            dialog.set_close_response("close");
            if available {
                dialog.set_response_appearance("release", adw::ResponseAppearance::Suggested);
            }
            let copy = ui.clone();
            dialog.connect_response(Some("release"), move |_, _| {
                if let Err(error) = open::that(&url) {
                    copy.toast(&format!("Could not open the release page: {error}"));
                }
            });
            dialog.present(Some(&ui.window));
        });
    }
}
