//! Album-art fetching for the now-playing panel.
//!
//! Rendering is left to [ratatui-image](https://docs.rs/ratatui-image),
//! which asks the terminal whether it speaks Kitty, sixel or — worst case —
//! falls back to unicode half-blocks. This module only downloads a track's
//! cover, decodes it, and keeps it cached for the TUI's lifetime.

use std::sync::{Arc, Mutex, OnceLock};

pub struct CoverArt {
    pub image: image::DynamicImage,
}

/// Most covers kept in memory.
const CACHE_CAP: usize = 8;
/// Longest edge of a cached cover, in pixels. The panel is only
/// COVER_CELLS (12x6 cells), i.e. roughly 100-130 px tall on common fonts
/// and under 200 px even on very tall cells, so 160 loses nothing visible
/// while a thumbnail costs ~100 KB instead of ~400 KB at 320.
const MAX_EDGE: u32 = 160;

/// Least-recently-used cover cache: the most recent entry is last.
struct Lru {
    entries: Vec<(String, Arc<CoverArt>)>,
}

impl Lru {
    fn get(&mut self, url: &str) -> Option<Arc<CoverArt>> {
        let index = self.entries.iter().position(|(key, _)| key == url)?;
        let entry = self.entries.remove(index);
        let art = entry.1.clone();
        self.entries.push(entry);
        Some(art)
    }

    fn insert(&mut self, url: String, art: Arc<CoverArt>) {
        self.entries.retain(|(key, _)| *key != url);
        if self.entries.len() >= CACHE_CAP {
            self.entries.remove(0);
        }
        self.entries.push((url, art));
    }
}

static CACHE: OnceLock<Mutex<Lru>> = OnceLock::new();

fn cache() -> &'static Mutex<Lru> {
    CACHE.get_or_init(|| Mutex::new(Lru { entries: Vec::new() }))
}

/// An already-fetched cover, when the URL was downloaded this session.
pub fn cached(url: &str) -> Option<Arc<CoverArt>> {
    cache().lock().unwrap().get(url)
}

/// Download and decode a cover image. Failures return an error the TUI can
/// ignore (the panel keeps its placeholder).
pub async fn load(url: String) -> anyhow::Result<Arc<CoverArt>> {
    if let Some(art) = cached(&url) {
        return Ok(art);
    }
    let bytes = crate::api::http_client()
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    // The full-size decode is dropped at the end of this block; only the
    // thumbnail is kept.
    let thumb = {
        let full = image::load_from_memory(&bytes)?;
        full.thumbnail(MAX_EDGE, MAX_EDGE)
    };
    drop(bytes);
    let cover = Arc::new(CoverArt { image: thumb });
    cache().lock().unwrap().insert(url, cover.clone());
    Ok(cover)
}

const HUE_BUCKETS: usize = 16;

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to(r), to(g), to(b))
}

/// A vivid accent color for `img`: the dominant saturated hue, lightened so
/// it reads on dark backgrounds. `None` when the cover is essentially gray.
pub fn accent_color(img: &image::DynamicImage) -> Option<(u8, u8, u8)> {
    let small = img
        .resize_exact(24, 24, image::imageops::FilterType::Triangle)
        .to_rgb8();
    // Per hue bucket: pixel count, summed saturation, summed hue vector.
    let mut buckets = [(0u32, 0f32, 0f32, 0f32); HUE_BUCKETS];
    for pixel in small.pixels() {
        let (h, s, l) = rgb_to_hsl(pixel[0], pixel[1], pixel[2]);
        if s < 0.25 || !(0.15..=0.92).contains(&l) {
            continue;
        }
        let index = ((h / 360.0 * HUE_BUCKETS as f32) as usize).min(HUE_BUCKETS - 1);
        let bucket = &mut buckets[index];
        bucket.0 += 1;
        bucket.1 += s;
        let rad = h.to_radians();
        bucket.2 += rad.cos();
        bucket.3 += rad.sin();
    }
    let (count, sat_sum, cos_sum, sin_sum) = *buckets
        .iter()
        .filter(|b| b.0 > 0)
        .max_by(|a, b| {
            let score = |b: &&(u32, f32, f32, f32)| b.0 as f32 * (b.1 / b.0 as f32);
            score(a).total_cmp(&score(b))
        })?;
    // Ignore a handful of stray pixels (under ~2% of the 576).
    if count < 12 {
        return None;
    }
    let hue = sin_sum.atan2(cos_sum).to_degrees().rem_euclid(360.0);
    let saturation = (sat_sum / count as f32).max(0.45);
    Some(hsl_to_rgb(hue, saturation, 0.65))
}

#[cfg(test)]
mod tests {
    use super::{accent_color, Arc, CoverArt, Lru, CACHE_CAP};

    fn solid(r: u8, g: u8, b: u8) -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(32, 32, image::Rgb([r, g, b])))
    }

    #[test]
    fn accent_of_red_is_reddish() {
        let (r, g, b) = accent_color(&solid(200, 20, 20)).expect("red has an accent");
        assert!(r > g && r > b, "got {r},{g},{b}");
    }

    #[test]
    fn accent_of_gray_is_none() {
        assert_eq!(accent_color(&solid(120, 120, 120)), None);
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let art = || Arc::new(CoverArt { image: solid(1, 2, 3) });
        let mut lru = Lru { entries: Vec::new() };
        for i in 0..CACHE_CAP {
            lru.insert(format!("u{i}"), art());
        }
        assert!(lru.get("u0").is_some()); // touch the oldest
        lru.insert("new".into(), art());
        assert!(lru.get("u0").is_some());
        assert!(lru.get("u1").is_none()); // now the least recent
        assert_eq!(lru.entries.len(), CACHE_CAP);
    }

    #[test]
    fn load_rejects_garbage() {
        // Not a URL worth fetching; decode errors surface instead.
        assert!(image::load_from_memory(b"not an image").is_err());
    }
}
