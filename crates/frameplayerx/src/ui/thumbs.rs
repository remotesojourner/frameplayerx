//! Thumbnail textures: decoded on worker threads, uploaded into one egui
//! context, kept in a small LRU. Server pictures come through the
//! [`ImageCache`].

use crate::image_cache::ImageCache;
use crate::services::Opener;
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

const MAX_TEXTURES: usize = 240;
const MAX_WIDTH: u32 = 480;
/// Ends the key of a seek-bar sheet (see [`Thumbs::get_sheet`]).
const SHEET: &str = "#sheet";
/// Largest side a seek-bar sheet is decoded at: the panels' largest texture
/// ([`crate::world::MAX_TEXTURE_SIDE`]). DeoVR's sheets are 4096 wide, so
/// their tiles are kept pixel for pixel.
const SHEET_SIDE: u32 = crate::world::MAX_TEXTURE_SIDE as u32;
/// Width of the decodes banners across the page use.
const WIDE_WIDTH: u32 = 1280;

pub struct Thumbs {
    textures: HashMap<String, TextureHandle>,
    order: VecDeque<String>,
    pending: HashSet<String>,
    failed: HashSet<String>,
    /// A feed's seek-bar sheet, decoded but not uploaded: the seek bar is in
    /// another panel, with its own egui context, so it uploads this itself.
    /// Taken when it does, so the decode isn't held as well.
    sheet: Option<(String, Arc<ColorImage>)>,
    /// (texture key, path or URL, width)
    req: Sender<(String, String, u32)>,
    done: Receiver<(String, Option<ColorImage>)>,
}

/// Decodes a seek-bar sheet to fit in [`SHEET_SIDE`] square. Its
/// `source_size` is the file's size in pixels, which the sheet's tile size
/// is given in.
fn decode_sheet(bytes: &[u8]) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let full = egui::vec2(img.width() as f32, img.height() as f32);
    let img = if img.width() > SHEET_SIDE || img.height() > SHEET_SIDE {
        img.resize(
            SHEET_SIDE,
            SHEET_SIDE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some(
        ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            rgba.as_raw(),
        )
        .with_source_size(full),
    )
}

/// Decodes `bytes` for the texture under `key`.
fn decode_for(key: &str, bytes: &[u8], max_width: u32) -> Option<ColorImage> {
    if key.ends_with(SHEET) {
        decode_sheet(bytes)
    } else {
        decode(bytes, max_width)
    }
}

/// Decodes an image file into an egui image no wider than `max_width`.
pub fn decode(bytes: &[u8], max_width: u32) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > max_width {
        let h = (img.height() as u64 * max_width as u64 / img.width() as u64).max(1) as u32;
        img.resize_exact(max_width, h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

impl Thumbs {
    /// `key`s are local paths or URLs.
    pub fn new(opener: Arc<Opener>, images: Arc<ImageCache>) -> Thumbs {
        let (req, rx) = unbounded::<(String, String, u32)>();
        let (tx, done) = unbounded();
        for i in 0..3 {
            let rx = rx.clone();
            let tx = tx.clone();
            let opener = opener.clone();
            let images = images.clone();
            let _ = std::thread::Builder::new()
                .name(format!("fp-thumbs-{i}"))
                .spawn(move || {
                    for (key, url, width) in rx.iter() {
                        if !fp_sources::urlutil::is_http_url(&url)
                            && !fp_sources::is_source_location(&url)
                        {
                            let img = opener
                                .fetch(&url, 16 << 20)
                                .ok()
                                .and_then(|b| decode_for(&key, &b, width));
                            if tx.send((key, img)).is_err() {
                                break;
                            }
                            continue;
                        }
                        let (img, due) = match images.get(&url) {
                            Ok((bytes, due)) => (decode_for(&key, &bytes, width), due),
                            Err(_) => (None, false),
                        };
                        if tx.send((key.clone(), img)).is_err() {
                            break;
                        }
                        // Shown from the cache first; a changed picture
                        // replaces it.
                        if due
                            && let Ok(Some(bytes)) = images.refresh(&url)
                            && let Some(img) = decode_for(&key, &bytes, width)
                            && tx.send((key, Some(img))).is_err()
                        {
                            break;
                        }
                    }
                });
        }
        Thumbs {
            textures: HashMap::new(),
            order: VecDeque::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            sheet: None,
            req,
            done,
        }
    }

    /// Uploads finished decodes; true when something new is available.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut any = false;
        for (key, img) in self.done.try_iter() {
            self.pending.remove(&key);
            match img {
                Some(img) if key.ends_with(SHEET) => {
                    self.sheet = Some((key, Arc::new(img)));
                    any = true;
                }
                Some(img) => {
                    let tex = ctx.load_texture(&key, img, TextureOptions::LINEAR);
                    if self.textures.insert(key.clone(), tex).is_none() {
                        self.order.push_back(key);
                    }
                    any = true;
                }
                None => {
                    self.failed.insert(key);
                }
            }
        }
        while self.order.len() > MAX_TEXTURES {
            if let Some(old) = self.order.pop_front() {
                self.textures.remove(&old);
            }
        }
        any
    }

    /// The texture for `key`, requesting it when missing.
    pub fn get(&mut self, key: &str) -> Option<TextureHandle> {
        self.lookup(key, key, MAX_WIDTH)
    }

    /// A larger decode of `key` for a banner across the page; the small
    /// one (when there is one) until it is ready.
    pub fn get_wide(&mut self, key: &str) -> Option<TextureHandle> {
        self.lookup(&format!("{key}#wide"), key, WIDE_WIDTH)
            .or_else(|| self.textures.get(key).cloned())
    }

    /// A feed's seek-bar preview sheet, decoded (see [`decode_sheet`]), for
    /// the caller to upload into its own egui context; asked for when
    /// neither here nor on its way. Handed over once.
    pub fn get_sheet(&mut self, url: &str) -> Option<Arc<ColorImage>> {
        let key = format!("{url}{SHEET}");
        if let Some(img) = self.take_sheet(&key) {
            return Some(img);
        }
        if !self.pending.contains(&key) && !self.failed.contains(&key) {
            self.pending.insert(key.clone());
            let _ = self.req.send((key, url.to_string(), SHEET_SIDE));
        }
        None
    }

    /// A newer decode of a sheet [`Self::get_sheet`] already handed over:
    /// the server's picture changed since it was cached.
    pub fn sheet_update(&mut self, url: &str) -> Option<Arc<ColorImage>> {
        self.take_sheet(&format!("{url}{SHEET}"))
    }

    fn take_sheet(&mut self, key: &str) -> Option<Arc<ColorImage>> {
        if self.sheet.as_ref().is_some_and(|(k, _)| k == key) {
            self.sheet.take().map(|(_, img)| img)
        } else {
            None
        }
    }

    /// The texture under `key`, decoding `url` at `width` when missing.
    fn lookup(&mut self, key: &str, url: &str, width: u32) -> Option<TextureHandle> {
        if let Some(t) = self.textures.get(key) {
            // Refresh LRU position.
            if let Some(pos) = self.order.iter().position(|k| k == key)
                && pos + 32 < self.order.len()
            {
                let k = self.order.remove(pos).unwrap_or_default();
                self.order.push_back(k);
            }
            return Some(t.clone());
        }
        if !self.pending.contains(key) && !self.failed.contains(key) {
            self.pending.insert(key.to_string());
            let _ = self.req.send((key.to_string(), url.to_string(), width));
        }
        None
    }

    /// Forget failures (e.g. after the library re-thumbnailed).
    pub fn retry_failed(&mut self) {
        self.failed.clear();
    }

    /// Drops what was decoded of `key`, so it is read again: the file was
    /// written anew under the same name.
    pub fn forget(&mut self, key: &str) {
        let wide = format!("{key}#wide");
        for k in [key, wide.as_str()] {
            self.textures.remove(k);
            self.failed.remove(k);
        }
        self.order.retain(|k| k != key && *k != wide);
    }

    /// Drops every server picture, so each is fetched again.
    pub fn forget_remote(&mut self) {
        let remote =
            |k: &String| fp_sources::urlutil::is_http_url(k) || fp_sources::is_source_location(k);
        self.textures.retain(|k, _| !remote(k));
        self.failed.retain(|k| !remote(k));
        self.order.retain(|k| !remote(k));
    }
}

/// Paints `tex` filling `rect` (cropped to its aspect) with rounded corners.
pub fn paint_cover_rounded(
    painter: &egui::Painter,
    rect: egui::Rect,
    tex: &TextureHandle,
    radius: u8,
) {
    painter.add(
        egui::epaint::RectShape::filled(rect, radius, egui::Color32::WHITE)
            .with_texture(tex.id(), cover_uv(rect, tex)),
    );
}

/// The part of the texture that fills `rect`, cropped to its aspect.
fn cover_uv(rect: egui::Rect, tex: &TextureHandle) -> egui::Rect {
    let [w, h] = tex.size();
    let r_aspect = rect.width() / rect.height().max(1.0);
    let t_aspect = w as f32 / (h as f32).max(1.0);
    if t_aspect > r_aspect {
        let f = r_aspect / t_aspect;
        egui::Rect::from_min_max(
            egui::pos2(0.5 - f / 2.0, 0.0),
            egui::pos2(0.5 + f / 2.0, 1.0),
        )
    } else {
        let f = t_aspect / r_aspect;
        egui::Rect::from_min_max(
            egui::pos2(0.0, 0.5 - f / 2.0),
            egui::pos2(1.0, 0.5 + f / 2.0),
        )
    }
}
