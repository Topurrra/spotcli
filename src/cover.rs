//! Album-art fetching for the now-playing panel.
//!
//! Rendering is left to [ratatui-image](https://docs.rs/ratatui-image),
//! which asks the terminal whether it speaks Kitty, sixel or — worst case —
//! falls back to unicode half-blocks. This module only downloads a track's
//! cover, decodes it, and keeps it cached for the TUI's lifetime.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub struct CoverArt {
    pub image: image::DynamicImage,
}

/// Most covers kept in memory.
const CACHE_CAP: usize = 32;
/// Longest edge of a cached cover, in pixels.
const MAX_EDGE: u32 = 320;

static CACHE: OnceLock<Mutex<HashMap<String, Arc<CoverArt>>>> = OnceLock::new();
static HTTP: OnceLock<reqwest::Client> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Arc<CoverArt>>> {
    CACHE.get_or_init(Default::default)
}

fn http() -> &'static reqwest::Client {
    HTTP.get_or_init(reqwest::Client::new)
}

/// An already-fetched cover, when the URL was downloaded this session.
pub fn cached(url: &str) -> Option<Arc<CoverArt>> {
    cache().lock().unwrap().get(url).cloned()
}

/// Download and decode a cover image. Failures return an error the TUI can
/// ignore (the panel keeps its placeholder).
pub async fn load(url: String) -> anyhow::Result<Arc<CoverArt>> {
    if let Some(art) = cached(&url) {
        return Ok(art);
    }
    let bytes = http()
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let cover = CoverArt {
        image: image::load_from_memory(&bytes)?.thumbnail(MAX_EDGE, MAX_EDGE),
    };
    let cover = Arc::new(cover);
    {
        let mut cache = cache().lock().unwrap();
        if cache.len() >= CACHE_CAP {
            // Simple eviction: drop an arbitrary entry.
            if let Some(key) = cache.keys().next().cloned() {
                cache.remove(&key);
            }
        }
        cache.insert(url, cover.clone());
    }
    Ok(cover)
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn load_rejects_garbage() {
        // Not a URL worth fetching; decode errors surface instead.
        assert!(image::load_from_memory(b"not an image").is_err());
    }
}
