mod api;
mod app;
mod auth;
mod models;
mod theme;
pub mod theme_detect;
mod ui;

pub use api::Client;
pub use app::App;
pub use auth::{login, StoredCredentials, TokenStore};
pub use models::{Device, Playback, Playlist, Track, User};
pub use theme::Theme;

use anyhow::{Context, Result};

/// Runtime directory for credentials and configuration.
pub fn config_dir() -> Result<std::path::PathBuf> {
    dirs::home_dir()
        .map(|home| home.join(".config").join("spotcli"))
        .context("could not determine a home directory")
}
