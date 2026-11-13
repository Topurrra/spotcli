//! Preferences remembered between runs, stored as `settings.json` in the
//! config directory.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const FILE: &str = "settings.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The theme name last chosen in the TUI (one of `theme::NAMES`).
    pub theme: Option<String>,
}

/// Read the settings from `dir`. A missing or unreadable file, or one that
/// does not parse, yields the defaults; this never fails.
pub fn load(dir: &Path) -> Settings {
    std::fs::read(dir.join(FILE))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

/// Write the settings to `dir/settings.json` atomically: the content goes to
/// a temporary file that is then renamed over the real one, so a crash never
/// leaves a half-written file behind.
pub fn save(dir: &Path, settings: &Settings) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(FILE);
    let tmp = dir.join(format!("{FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(settings)?;
    {
        let mut file = std::fs::File::create(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "spotcli-settings-test-{}-{nanos}",
            std::process::id()
        ));

        assert_eq!(load(&dir), Settings::default(), "missing file");

        let saved = Settings {
            theme: Some("gruvbox".to_string()),
        };
        save(&dir, &saved).expect("save");
        assert_eq!(load(&dir), saved);
        assert!(!dir.join("settings.json.tmp").exists(), "tmp file renamed away");

        save(&dir, &Settings::default()).expect("overwrite");
        assert_eq!(load(&dir), Settings::default());

        std::fs::write(dir.join(FILE), b"{ not json").expect("write corrupt");
        assert_eq!(load(&dir), Settings::default(), "corrupt file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = std::env::temp_dir().join(format!(
            "spotcli-settings-future-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join(FILE), br#"{"theme":"dark","volume":3}"#).expect("write");
        assert_eq!(load(&dir).theme.as_deref(), Some("dark"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
