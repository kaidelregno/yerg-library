//! Album art textures, decoded off the UI thread and cached by hash.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::{ColorImage, TextureHandle, TextureOptions};
use yerg_core::Library;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    hash: String,
    full: bool,
}

enum Loaded {
    Image(ColorImage),
    Missing,
}

pub struct ArtCache {
    textures: HashMap<Key, Option<TextureHandle>>,
    pending: HashSet<Key>,
    req: Sender<Key>,
    res: Receiver<(Key, Loaded)>,
    /// Keep memory bounded: evict full-size images beyond this many.
    full_order: Vec<Key>,
}

impl ArtCache {
    pub fn new(db: PathBuf, ctx: egui::Context) -> ArtCache {
        let (req_tx, req_rx) = unbounded::<Key>();
        let (res_tx, res_rx) = unbounded::<(Key, Loaded)>();
        std::thread::Builder::new()
            .name("yerg-art".into())
            .spawn(move || worker(db, req_rx, res_tx, ctx))
            .expect("spawn art thread");
        ArtCache {
            textures: HashMap::new(),
            pending: HashSet::new(),
            req: req_tx,
            res: res_rx,
            full_order: Vec::new(),
        }
    }

    /// Texture for an art hash.  `None` while loading or when there is no image.
    pub fn get(&mut self, hash: Option<&str>, full: bool) -> Option<TextureHandle> {
        let hash = hash?;
        let key = Key {
            hash: hash.to_string(),
            full,
        };
        if let Some(t) = self.textures.get(&key) {
            return t.clone();
        }
        if self.pending.insert(key.clone()) {
            let _ = self.req.send(key);
        }
        None
    }

    /// Upload finished decodes.  Returns true if anything changed.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        while let Ok((key, loaded)) = self.res.try_recv() {
            self.pending.remove(&key);
            let tex = match loaded {
                Loaded::Image(img) => {
                    let name = format!("art-{}-{}", if key.full { "full" } else { "thumb" }, key.hash);
                    Some(ctx.load_texture(name, img, TextureOptions::LINEAR))
                }
                Loaded::Missing => None,
            };
            if key.full {
                self.full_order.push(key.clone());
                while self.full_order.len() > 8 {
                    let old = self.full_order.remove(0);
                    self.textures.remove(&old);
                }
            }
            self.textures.insert(key, tex);
            changed = true;
        }
        changed
    }

    pub fn forget(&mut self, hash: &str) {
        self.textures.retain(|k, _| k.hash != hash);
    }
}

fn worker(db: PathBuf, rx: Receiver<Key>, tx: Sender<(Key, Loaded)>, ctx: egui::Context) {
    let Ok(lib) = Library::open(&db) else { return };
    for key in rx {
        let loaded = decode(&lib, &key).unwrap_or(Loaded::Missing);
        if tx.send((key, loaded)).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}

fn decode(lib: &Library, key: &Key) -> Option<Loaded> {
    let bytes = if key.full {
        lib.art(&key.hash).ok().flatten().map(|(_, d)| d)?
    } else {
        lib.art_thumb(&key.hash).ok().flatten()?
    };
    let img = image::load_from_memory(&bytes).ok()?;
    let img = if key.full && (img.width() > 1024 || img.height() > 1024) {
        img.thumbnail(1024, 1024)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(Loaded::Image(ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    )))
}
