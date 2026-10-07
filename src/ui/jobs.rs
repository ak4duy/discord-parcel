use super::{Outcome, Ui};
use adw::prelude::*;
use anyhow::Result;
use discord_parcel::parcel::{self, Cancel, Progress};
use gtk::glib;
use std::rc::Rc;

enum Event<T> {
    Progress(Progress),
    Finished(std::result::Result<T, String>),
}

impl Ui {
    pub(super) fn run_job<F>(self: &Rc<Self>, work: F)
    where
        F: FnOnce(&Cancel, &dyn Fn(Progress)) -> Result<Outcome> + Send + 'static,
    {
        self.run_job_with_completion(work, |ui, outcome| ui.show_outcome(outcome));
    }

    pub(super) fn run_job_with_completion<T, F, C>(self: &Rc<Self>, work: F, complete: C)
    where
        T: Send + 'static,
        F: FnOnce(&Cancel, &dyn Fn(Progress)) -> Result<T> + Send + 'static,
        C: FnOnce(&Rc<Self>, T) + 'static,
    {
        if self.busy.replace(true) {
            return;
        }
        self.controls.set_sensitive(false);
        self.connection_button.set_sensitive(false);
        self.progress_box.set_visible(true);
        self.progress_bar.set_fraction(0.0);
        self.progress_title.set_label("Preparing…");
        self.progress_detail.set_label("");
        self.cancel_button.set_sensitive(true);
        self.spinner.start();
        let cancel = Cancel::default();
        self.cancel.replace(cancel.clone());
        let (sender, receiver) = async_channel::bounded(32);
        std::thread::spawn(move || {
            let progress = |progress| {
                let _ = sender.try_send(Event::Progress(progress));
            };
            let result = work(&cancel, &progress).map_err(|error| format!("{error:#}"));
            let _ = sender.send_blocking(Event::Finished(result));
        });
        let ui = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    Event::Progress(p) => {
                        ui.progress_title.set_label(&p.stage);
                        if p.total > 0 {
                            ui.progress_bar
                                .set_fraction((p.done as f64 / p.total as f64).clamp(0.0, 1.0));
                            ui.progress_detail.set_label(&format!(
                                "{} of {}",
                                parcel::human_size(p.done),
                                parcel::human_size(p.total)
                            ));
                        } else {
                            ui.progress_bar.pulse();
                            ui.progress_detail.set_label("Please wait…");
                        }
                    }
                    Event::Finished(result) => {
                        ui.busy.set(false);
                        ui.controls.set_sensitive(true);
                        ui.connection_button.set_sensitive(true);
                        ui.spinner.stop();
                        ui.progress_box.set_visible(false);

                        match result {
                            Ok(outcome) => complete(&ui, outcome),
                            Err(error) => {
                                let stopped = ui.cancel.borrow().check().is_err();
                                ui.show_error(stopped, &error);
                            }
                        }

                        break;
                    }
                }
            }
        });
    }
}
