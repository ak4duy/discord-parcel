#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod platform;
mod ui;

use adw::prelude::*;

fn main() -> gtk::glib::ExitCode {
    if let Err(error) = platform::initialize() {
        platform::startup_error(&format!("Discord Parcel could not start.\n\n{error:#}"));
        return gtk::glib::ExitCode::FAILURE;
    }
    let app = adw::Application::builder()
        .application_id("dev.akaduy.DiscordParcel")
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_activate(|app| ui::present(app, None));
    app.connect_open(|app, files, _| {
        ui::present(app, files.first().and_then(gtk::gio::File::path))
    });
    app.run()
}
