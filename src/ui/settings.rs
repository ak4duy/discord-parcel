use super::Ui;

use discord_parcel::parcel;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Settings {
    pub(super) channel_id: String,
    #[serde(default)]
    pub(super) token: String,
    #[serde(default = "default_chunk", alias = "chunk_mib")]
    pub(super) chunk_mb: u32,
    #[serde(default = "default_low_disk")]
    pub(super) low_disk: bool,
}

fn default_chunk() -> u32 {
    20
}

fn default_low_disk() -> bool {
    true
}

impl Settings {
    pub(super) fn load() -> Self {
        parcel::read_bounded(&parcel::data_dir().join("settings.json"), 32 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Settings {
                channel_id: String::new(),
                token: String::new(),
                chunk_mb: default_chunk(),
                low_disk: default_low_disk(),
            })
    }
}

impl Ui {
    pub(super) fn save_settings(&self) {
        let settings = Settings {
            channel_id: self.connection.borrow().channel_id.clone(),
            token: self.connection.borrow().token.clone(),
            chunk_mb: self.chunk_size.value_as_int() as u32,
            low_disk: self.low_disk.is_active(),
        };
        let result = serde_json::to_vec_pretty(&settings)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| {
                parcel::atomic_write(&parcel::data_dir().join("settings.json"), &bytes)
            });
        if let Err(error) = result {
            self.toast(&format!("Could not save preferences: {error}"));
        }
    }
}
