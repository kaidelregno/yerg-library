//! Appearance from dotfiles, the way Linux "ricing" works.
//!
//! Everything about the look lives in plain-text TOML files the user edits,
//! keeps in a dotfiles repository and shares:
//!
//! ```text
//! ~/.config/yerg/            (%APPDATA%\yerg on Windows, or $YERG_CONFIG_DIR)
//!   yerg.toml                which theme, fonts, sizes, icons, colour overrides
//!   themes/<name>.toml       colour schemes (several are written on first run)
//!   icons/<name>.svg|png     optional icon replacements
//! ```
//!
//! Like Alacritty and spotify-player: the config names a theme, `import`
//! loads more files in order, later values win, and the config's own sections
//! win last.  `theme = "wal"` takes the colours from pywal
//! (`~/.cache/wal/colors.json`).  Files are watched and changes apply as soon
//! as they are saved; mistakes are reported (Settings › Appearance and a
//! notice) instead of being silently ignored, and everything that did parse
//! still applies.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, SystemTime};

use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, FontId, TextStyle};
use serde::Deserialize;

/// The font family used for icon glyphs (`[icons] font` and `[icons.glyphs]`).
pub const ICON_FAMILY: &str = "yerg-icons";

/// Names of the icons that can be replaced, with what they are.
pub const ICON_NAMES: &[(&str, &str)] = &[
    ("play", "play button"),
    ("pause", "pause button"),
    ("previous", "previous track"),
    ("next", "next track"),
    ("shuffle", "shuffle"),
    ("repeat", "repeat"),
    ("repeat_one", "repeat one"),
    ("queue", "Up Next button"),
    ("volume", "volume"),
    ("volume_muted", "volume when muted"),
    ("sidebar_left", "hide/show the sidebar"),
    ("sidebar_right", "hide/show the right panel"),
    ("star", "rating star, filled"),
    ("star_empty", "rating star, empty"),
    ("now_playing", "marks the playing track in lists"),
];

// ------------------------------------------------------------------ files

/// Where the dotfiles live.
pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("YERG_CONFIG_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(a) = std::env::var("APPDATA") {
            return PathBuf::from(a).join("yerg");
        }
    }
    #[cfg(not(windows))]
    {
        // XDG on Linux, and on macOS too: that is where ricers keep dotfiles.
        if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
            if !x.is_empty() {
                return PathBuf::from(x).join("yerg");
            }
        }
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join(".config").join("yerg");
        }
    }
    PathBuf::from(".yerg")
}

pub fn config_path() -> PathBuf {
    config_dir().join("yerg.toml")
}

pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn wal_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".cache").join("wal").join("colors.json"))
}

/// `~/x` and paths relative to the config folder.
fn resolve_path(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(h) = home_dir() {
            return h.join(rest);
        }
    }
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        config_dir().join(path)
    }
}

// ------------------------------------------------------------- file format

macro_rules! colour_roles {
    ($($name:ident: $doc:literal),* $(,)?) => {
        /// `[colors]`: every role is optional, so a file can change just a few.
        #[derive(Debug, Clone, Default, Deserialize)]
        #[serde(default)]
        pub struct ColorSection {
            /// Base the defaults on egui's dark (true) or light (false) look.
            pub dark: Option<bool>,
            $(#[doc = $doc] pub $name: Option<String>,)*
        }

        impl ColorSection {
            fn overlay(&mut self, top: &ColorSection) {
                if top.dark.is_some() {
                    self.dark = top.dark;
                }
                $(if top.$name.is_some() { self.$name = top.$name.clone(); })*
            }
        }

        /// Every colour role name, for typo reports.
        pub const ROLE_NAMES: &[&str] = &[$(stringify!($name)),*];

        /// The resolved colours.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Colors {
            pub dark: bool,
            $(#[doc = $doc] pub $name: Color32,)*
        }

        impl Colors {
            /// `base` filled in with whatever `section` sets.
            fn resolve(base: &Colors, section: &ColorSection, errors: &mut Vec<String>, source: &str) -> Colors {
                Colors {
                    dark: section.dark.unwrap_or(base.dark),
                    $($name: match &section.$name {
                        Some(v) => parse_colour(v).unwrap_or_else(|e| {
                            errors.push(format!("{source}: colors.{}: {e}", stringify!($name)));
                            base.$name
                        }),
                        None => base.$name,
                    },)*
                }
            }
        }
    };
}

colour_roles! {
    background: "Main window background.",
    surface: "Dialogs, menus and alternate table rows.",
    raised: "Placeholders (missing cover art) and cards.",
    sunken: "Text field background.",
    hover: "Background under the pointer.",
    line: "Separators and borders.",
    text: "Normal text.",
    strong: "Headings and emphasised text.",
    weak: "Captions and secondary text.",
    icon: "Transport and panel icons.",
    icon_dim: "Inactive parts of icons (low volume waves).",
    track: "Unfilled part of the seek and volume bars.",
    knob: "Seek and volume knobs.",
    accent: "Active toggles, filled bars, the playing marker.",
    playing: "Text of the playing track in lists.",
    selection: "Selected rows and highlights.",
    selection_text: "Text on selected rows.",
    stars: "Filled rating stars.",
    stars_empty: "Empty rating stars.",
    link: "Links (artists, albums).",
    warning: "Warnings and waiting states.",
    error: "Errors and destructive actions.",
    success: "Connected and done states.",
}

/// `[font]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FontSection {
    /// An installed font family, e.g. "Inter" or "JetBrainsMono Nerd Font".
    pub family: Option<String>,
    /// Or a .ttf / .otf file (absolute, `~/`, or relative to the config folder).
    pub file: Option<String>,
    pub monospace: Option<String>,
    pub monospace_file: Option<String>,
    /// Body text size in points.
    pub size: Option<f32>,
    pub small: Option<f32>,
    pub heading: Option<f32>,
    pub mono_size: Option<f32>,
    /// Zoom for the whole interface (1.0 = 100%).
    pub scale: Option<f32>,
}

impl FontSection {
    fn overlay(&mut self, top: &FontSection) {
        // A family in a later file replaces a file from an earlier one, and vice versa.
        if top.family.is_some() || top.file.is_some() {
            self.family = top.family.clone();
            self.file = top.file.clone();
        }
        if top.monospace.is_some() || top.monospace_file.is_some() {
            self.monospace = top.monospace.clone();
            self.monospace_file = top.monospace_file.clone();
        }
        macro_rules! take {
            ($($f:ident),*) => { $(if top.$f.is_some() { self.$f = top.$f; })* };
        }
        take!(size, small, heading, mono_size, scale);
    }
}

/// `[icons]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IconSection {
    /// Size multiplier for the transport and panel icons.
    pub size: Option<f32>,
    /// Folder of replacement icons named like `play.svg` or `queue.png`.
    pub folder: Option<String>,
    /// Colour image icons with the theme's icon colours (for single-colour icons).
    pub tint: Option<bool>,
    /// Font for `glyphs`, e.g. "Symbols Nerd Font".
    pub font: Option<String>,
    pub font_file: Option<String>,
    /// Icons drawn as text, e.g. `play = "\u{f04b}"` with a Nerd Font.
    pub glyphs: BTreeMap<String, String>,
}

impl IconSection {
    fn overlay(&mut self, top: &IconSection) {
        if top.size.is_some() {
            self.size = top.size;
        }
        if top.folder.is_some() {
            self.folder = top.folder.clone();
        }
        if top.tint.is_some() {
            self.tint = top.tint;
        }
        if top.font.is_some() || top.font_file.is_some() {
            self.font = top.font.clone();
            self.font_file = top.font_file.clone();
        }
        for (k, v) in &top.glyphs {
            self.glyphs.insert(k.clone(), v.clone());
        }
    }
}

/// A theme or imported file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ThemeFile {
    /// Shown in Settings; defaults to the file name.
    name: Option<String>,
    colors: ColorSection,
    font: FontSection,
    icons: IconSection,
}

/// `yerg.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    theme: Option<String>,
    import: Vec<String>,
    colors: ColorSection,
    font: FontSection,
    icons: IconSection,
}

fn parse_colour(s: &str) -> Result<Color32, String> {
    let h = s.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| format!("\"{s}\" is not a hex colour"));
    let nib = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).map(|v| v * 17).map_err(|_| format!("\"{s}\" is not a hex colour"));
    if !h.is_ascii() {
        return Err(format!("\"{s}\" is not a hex colour"));
    }
    match h.len() {
        3 => Ok(Color32::from_rgb(nib(0)?, nib(1)?, nib(2)?)),
        6 => Ok(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Ok(Color32::from_rgba_unmultiplied(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => Err(format!("\"{s}\" is not a hex colour (use #rgb, #rrggbb or #rrggbbaa)")),
    }
}

// --------------------------------------------------------- bundled themes

/// The look the app shipped with (and the fallback for missing roles).
pub const THEME_DEFAULT: &str = r##"# Yerg Library's own dark theme.
name = "Default"

[colors]
dark = true
background = "#18191c"
surface = "#1e1f23"
raised = "#282a30"
sunken = "#121316"
hover = "#2c2e34"
line = "#3c3d42"
text = "#b9b9b9"
strong = "#ffffff"
weak = "#8c8c8c"
icon = "#c8c8c8"
icon_dim = "#6e6e6e"
track = "#464646"
knob = "#ffffff"
accent = "#4a90d9"
playing = "#78aaeb"
selection = "#3b7dd8"
selection_text = "#ffffff"
stars = "#e6be3c"
stars_empty = "#505050"
link = "#96bef0"
warning = "#e6be5a"
error = "#e66e6e"
success = "#5ac878"
"##;

const THEME_LIGHT: &str = r##"name = "Light"

[colors]
dark = false
background = "#f4f4f6"
surface = "#ffffff"
raised = "#e4e4e8"
sunken = "#ffffff"
hover = "#e2e2e8"
line = "#d0d0d6"
text = "#2a2a2e"
strong = "#000000"
weak = "#6c6c72"
icon = "#3a3a40"
icon_dim = "#a0a0a6"
track = "#cfcfd6"
knob = "#2a2a2e"
accent = "#2f6fd6"
playing = "#2f6fd6"
selection = "#2f6fd6"
selection_text = "#ffffff"
stars = "#c8960a"
stars_empty = "#c4c4ca"
link = "#2f6fd6"
warning = "#b26a00"
error = "#c62828"
success = "#2e7d32"
"##;

const THEME_GRUVBOX: &str = r##"# https://github.com/morhetz/gruvbox
name = "Gruvbox Dark"

[colors]
dark = true
background = "#282828"
surface = "#32302f"
raised = "#3c3836"
sunken = "#1d2021"
hover = "#3c3836"
line = "#504945"
text = "#ebdbb2"
strong = "#fbf1c7"
weak = "#a89984"
icon = "#d5c4a1"
icon_dim = "#665c54"
track = "#504945"
knob = "#fbf1c7"
accent = "#83a598"
playing = "#8ec07c"
selection = "#458588"
selection_text = "#fbf1c7"
stars = "#fabd2f"
stars_empty = "#504945"
link = "#83a598"
warning = "#fe8019"
error = "#fb4934"
success = "#b8bb26"
"##;

const THEME_NORD: &str = r##"# https://www.nordtheme.com
name = "Nord"

[colors]
dark = true
background = "#2e3440"
surface = "#3b4252"
raised = "#434c5e"
sunken = "#272c36"
hover = "#434c5e"
line = "#4c566a"
text = "#d8dee9"
strong = "#eceff4"
weak = "#8a94a8"
icon = "#d8dee9"
icon_dim = "#616e88"
track = "#4c566a"
knob = "#eceff4"
accent = "#88c0d0"
playing = "#88c0d0"
selection = "#5e81ac"
selection_text = "#eceff4"
stars = "#ebcb8b"
stars_empty = "#4c566a"
link = "#81a1c1"
warning = "#d08770"
error = "#bf616a"
success = "#a3be8c"
"##;

const THEME_CATPPUCCIN: &str = r##"# https://catppuccin.com (Mocha)
name = "Catppuccin Mocha"

[colors]
dark = true
background = "#1e1e2e"
surface = "#181825"
raised = "#313244"
sunken = "#11111b"
hover = "#313244"
line = "#45475a"
text = "#cdd6f4"
strong = "#ffffff"
weak = "#a6adc8"
icon = "#bac2de"
icon_dim = "#6c7086"
track = "#45475a"
knob = "#cdd6f4"
accent = "#cba6f7"
playing = "#89b4fa"
selection = "#585b70"
selection_text = "#cdd6f4"
stars = "#f9e2af"
stars_empty = "#45475a"
link = "#89b4fa"
warning = "#fab387"
error = "#f38ba8"
success = "#a6e3a1"
"##;

const THEME_DRACULA: &str = r##"# https://draculatheme.com
name = "Dracula"

[colors]
dark = true
background = "#282a36"
surface = "#21222c"
raised = "#343746"
sunken = "#191a21"
hover = "#343746"
line = "#44475a"
text = "#f8f8f2"
strong = "#ffffff"
weak = "#9ea8c7"
icon = "#f8f8f2"
icon_dim = "#6272a4"
track = "#44475a"
knob = "#f8f8f2"
accent = "#bd93f9"
playing = "#ff79c6"
selection = "#44475a"
selection_text = "#f8f8f2"
stars = "#f1fa8c"
stars_empty = "#44475a"
link = "#8be9fd"
warning = "#ffb86c"
error = "#ff5555"
success = "#50fa7b"
"##;

/// Written to `themes/` on first run (existing files are never touched).
pub const BUNDLED_THEMES: &[(&str, &str)] = &[
    ("default", THEME_DEFAULT),
    ("light", THEME_LIGHT),
    ("gruvbox", THEME_GRUVBOX),
    ("nord", THEME_NORD),
    ("catppuccin-mocha", THEME_CATPPUCCIN),
    ("dracula", THEME_DRACULA),
];

/// The commented `yerg.toml` written on first run.
pub const DEFAULT_CONFIG: &str = r##"# Yerg Library appearance.
#
# Edit and save: changes apply right away.  Mistakes are listed in
# Settings > Appearance, and everything else keeps working.

# A theme from the themes/ folder next to this file (without .toml), or
# "wal" for pywal's colours (~/.cache/wal/colors.json).
theme = "default"

# More files to load, in order (later ones win; this file wins last).
# Paths are absolute, ~/..., or relative to this folder.
# import = ["themes/my-tweaks.toml"]

[font]
# An installed font family (Settings > Appearance lists them), or a file.
# family = "Inter"
# file = "fonts/Inter-Regular.ttf"
# monospace = "JetBrainsMono Nerd Font"
# size = 13.0        # body text, points
# small = 11.0       # captions
# heading = 20.0
# mono_size = 12.5
# scale = 1.0        # zoom the whole interface

[icons]
# size = 1.0         # multiplier for transport and panel icons
# Replacement icons: put play.svg, pause.png, ... in this folder.
# Names: play pause previous next shuffle repeat repeat_one queue volume
#        volume_muted sidebar_left sidebar_right star star_empty now_playing
# folder = "icons"
# tint = true        # colour single-colour icons with the theme
# Or draw icons as text with an icon font (e.g. a Nerd Font):
# font = "Symbols Nerd Font"
# [icons.glyphs]
# play = ""
# pause = ""

[colors]
# Override any colour of the theme here, e.g.
# accent = "#ff79c6"
# Roles: background surface raised sunken hover line text strong weak icon
#        icon_dim track knob accent playing selection selection_text stars
#        stars_empty link warning error success   (and dark = true/false)
"##;

const ICONS_README: &str = "Replacement icons for Yerg Library.\n\n\
Name a file after the icon it replaces, as .svg (best) or .png:\n\n\
  play pause previous next shuffle repeat repeat_one queue volume\n\
  volume_muted sidebar_left sidebar_right star star_empty now_playing\n\n\
Single-colour icons are coloured with the theme (see `tint` in yerg.toml).\n";

/// Create the config folder with the default config, the bundled themes and
/// the icons folder, leaving anything that already exists alone.
pub fn ensure_files() -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(dir.join("themes"))?;
    std::fs::create_dir_all(dir.join("icons"))?;
    let cfg = config_path();
    if !cfg.exists() {
        std::fs::write(&cfg, DEFAULT_CONFIG)?;
    }
    for (name, body) in BUNDLED_THEMES {
        let p = themes_dir().join(format!("{name}.toml"));
        if !p.exists() {
            std::fs::write(p, body)?;
        }
    }
    let readme = dir.join("icons").join("README.txt");
    if !readme.exists() {
        std::fs::write(readme, ICONS_README)?;
    }
    Ok(())
}

/// Theme names available: files in themes/ plus "wal" when pywal has run.
pub fn available_themes() -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(themes_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension().and_then(|x| x.to_str()) == Some("toml"))
                        .then(|| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
                        .flatten()
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    if wal_path().map(|p| p.is_file()).unwrap_or(false) {
        out.push("wal".into());
    }
    out
}

/// Point `theme = …` in yerg.toml at another theme, keeping the file's
/// comments and layout.
pub fn select_theme(name: &str) -> Result<(), String> {
    let path = config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("yerg.toml: {e}"))?;
    doc["theme"] = toml_edit::value(name);
    std::fs::write(&path, doc.to_string()).map_err(|e| format!("writing {}: {e}", path.display()))
}

// ------------------------------------------------------------------ fonts

#[derive(Debug, Clone)]
pub struct LoadedFont {
    /// What the user asked for, for messages and to tell changes apart.
    pub label: String,
    pub data: Arc<Vec<u8>>,
    pub index: u32,
}

static FONT_DB: OnceLock<fontdb::Database> = OnceLock::new();

fn system_fonts() -> &'static fontdb::Database {
    FONT_DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db
    })
}

/// Installed font families, for Settings › Appearance.
pub fn system_font_families() -> Vec<String> {
    let mut names: Vec<String> = system_fonts()
        .faces()
        .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

fn load_font(family: Option<&str>, file: Option<&str>, what: &str, errors: &mut Vec<String>) -> Option<LoadedFont> {
    if let Some(f) = file.filter(|f| !f.trim().is_empty()) {
        let path = resolve_path(f);
        return match std::fs::read(&path) {
            Ok(data) => Some(LoadedFont { label: format!("file {}", path.display()), data: Arc::new(data), index: 0 }),
            Err(e) => {
                errors.push(format!("{what}: cannot read {}: {e}", path.display()));
                None
            }
        };
    }
    let name = family.filter(|n| !n.trim().is_empty())?;
    let db = system_fonts();
    let query = fontdb::Query { families: &[fontdb::Family::Name(name)], ..Default::default() };
    let Some(id) = db.query(&query) else {
        errors.push(format!("{what}: no installed font family is called \"{name}\" (Settings › Appearance lists them)"));
        return None;
    };
    match db.with_face_data(id, |data, index| (data.to_vec(), index)) {
        Some((data, index)) => Some(LoadedFont { label: format!("family {name}"), data: Arc::new(data), index }),
        None => {
            errors.push(format!("{what}: could not read the font \"{name}\""));
            None
        }
    }
}

// ------------------------------------------------------------------ theme

/// Everything the UI reads.
#[derive(Debug, Clone)]
pub struct Theme {
    /// From the theme file, for Settings.
    pub name: String,
    pub colors: Colors,
    pub body_size: f32,
    pub small_size: f32,
    pub heading_size: f32,
    pub mono_size: f32,
    pub scale: f32,
    pub ui_font: Option<LoadedFont>,
    pub mono_font: Option<LoadedFont>,
    pub icon_font: Option<LoadedFont>,
    pub icon_scale: f32,
    pub icon_tint: bool,
    /// icon name -> replacement image.
    pub icon_files: HashMap<String, PathBuf>,
    /// icon name -> text drawn with [`ICON_FAMILY`].
    pub glyphs: HashMap<String, String>,
}

impl Default for Theme {
    fn default() -> Self {
        let mut errors = Vec::new();
        let file: ThemeFile = toml::from_str(THEME_DEFAULT).expect("built-in theme parses");
        let colors = Colors::resolve(&fallback_colours(), &file.colors, &mut errors, "default");
        Theme {
            name: "Default".into(),
            colors,
            body_size: 13.0,
            small_size: 11.0,
            heading_size: 20.0,
            mono_size: 12.5,
            scale: 1.0,
            ui_font: None,
            mono_font: None,
            icon_font: None,
            icon_scale: 1.0,
            icon_tint: true,
            icon_files: HashMap::new(),
            glyphs: HashMap::new(),
        }
    }
}

/// Last-resort colours if even the built-in theme were incomplete.
fn fallback_colours() -> Colors {
    let g = Color32::from_gray;
    Colors {
        dark: true,
        background: g(24),
        surface: g(30),
        raised: g(42),
        sunken: g(18),
        hover: g(46),
        line: g(60),
        text: g(185),
        strong: Color32::WHITE,
        weak: g(140),
        icon: g(200),
        icon_dim: g(110),
        track: g(70),
        knob: Color32::WHITE,
        accent: Color32::from_rgb(74, 144, 217),
        playing: Color32::from_rgb(120, 170, 235),
        selection: Color32::from_rgb(59, 125, 216),
        selection_text: Color32::WHITE,
        stars: Color32::from_rgb(230, 190, 60),
        stars_empty: g(80),
        link: Color32::from_rgb(150, 190, 240),
        warning: Color32::from_rgb(230, 190, 90),
        error: Color32::from_rgb(230, 110, 110),
        success: Color32::from_rgb(90, 200, 120),
    }
}

static CURRENT: RwLock<Option<Arc<Theme>>> = RwLock::new(None);

/// The theme in use.
pub fn current() -> Arc<Theme> {
    if let Some(t) = CURRENT.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return t.clone();
    }
    let t = Arc::new(Theme::default());
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(t.clone());
    t
}

fn set_current(t: Theme) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(t));
}

/// The result of reading the dotfiles.
pub struct Loaded {
    pub theme: Theme,
    /// Problems found; everything else still applied.
    pub errors: Vec<String>,
    /// Files and folders to watch for changes.
    pub watch: Vec<PathBuf>,
}

fn read_toml<T: for<'de> Deserialize<'de> + Default>(path: &Path, errors: &mut Vec<String>) -> Option<T> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            errors.push(format!("cannot read {}: {e}", path.display()));
            return None;
        }
    };
    parse_toml(&text, &path.display().to_string(), errors)
}

/// Parse a config or theme file.  Unknown keys (typos) are reported with a
/// suggestion and skipped, so one mistake costs only that line; a wrong
/// value type rejects the file (with toml's message, which names the line).
fn parse_toml<T: for<'de> Deserialize<'de>>(text: &str, source: &str, errors: &mut Vec<String>) -> Option<T> {
    let mut table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            errors.push(format!("{source}: {}", e.to_string().trim()));
            return None;
        }
    };
    strip_unknown_keys(&mut table, source, errors);
    match toml::Value::Table(table).try_into::<T>() {
        Ok(v) => Some(v),
        Err(e) => {
            errors.push(format!("{source}: {}", e.to_string().trim()));
            None
        }
    }
}

const TOP_KEYS: &[&str] = &["theme", "import", "name", "colors", "font", "icons"];
const FONT_KEYS: &[&str] = &["family", "file", "monospace", "monospace_file", "size", "small", "heading", "mono_size", "scale"];
const ICON_KEYS: &[&str] = &["size", "folder", "tint", "font", "font_file", "glyphs"];

fn strip_unknown_keys(table: &mut toml::Table, source: &str, errors: &mut Vec<String>) {
    let mut colour_keys: Vec<&str> = ROLE_NAMES.to_vec();
    colour_keys.push("dark");
    let mut report = |table: &mut toml::Table, prefix: &str, allowed: &[&str]| {
        let unknown: Vec<String> = table.keys().filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
        for k in unknown {
            table.remove(&k);
            let hint = closest(&k, allowed).map(|s| format!(" (did you mean \"{s}\"?)")).unwrap_or_default();
            errors.push(format!("{source}: unknown key \"{prefix}{k}\"{hint}, skipped"));
        }
    };
    report(table, "", TOP_KEYS);
    for (section, allowed) in [("colors", colour_keys.as_slice()), ("font", FONT_KEYS), ("icons", ICON_KEYS)] {
        if let Some(toml::Value::Table(t)) = table.get_mut(section) {
            report(t, &format!("{section}."), allowed);
        }
    }
}

/// The allowed key nearest to a typo, if it is plausibly the one meant.
fn closest<'a>(word: &str, candidates: &[&'a str]) -> Option<&'a str> {
    fn distance(a: &str, b: &str) -> usize {
        let b: Vec<char> = b.chars().collect();
        let mut prev: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut cur = vec![i + 1];
            for (j, cb) in b.iter().enumerate() {
                cur.push((prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
            }
            prev = cur;
        }
        prev[b.len()]
    }
    candidates
        .iter()
        .map(|c| (distance(word, c), *c))
        .filter(|(d, c)| *d <= (c.len() / 3).max(2))
        .min()
        .map(|(_, c)| c)
}

/// yerg.toml, keeping `theme` and `import` even if another part is invalid.
fn read_config(errors: &mut Vec<String>) -> ConfigFile {
    let path = config_path();
    if let Some(cfg) = read_toml::<ConfigFile>(&path, errors) {
        return cfg;
    }
    let table: Option<toml::Table> = std::fs::read_to_string(&path).ok().and_then(|t| t.parse().ok());
    let Some(table) = table else { return ConfigFile::default() };
    ConfigFile {
        theme: table.get("theme").and_then(|v| v.as_str()).map(str::to_string),
        import: table
            .get("import")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        ..Default::default()
    }
}

/// pywal's colors.json as a theme.
fn wal_theme(path: &Path, errors: &mut Vec<String>) -> Option<ColorSection> {
    #[derive(Deserialize)]
    struct Wal {
        special: HashMap<String, String>,
        colors: HashMap<String, String>,
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| errors.push(format!("theme \"wal\": cannot read {}: {e} (run wal first)", path.display())))
        .ok()?;
    let wal: Wal = serde_json::from_str(&text)
        .map_err(|e| errors.push(format!("{}: {e}", path.display())))
        .ok()?;
    let c = |k: &str| wal.colors.get(k).cloned();
    let bg = wal.special.get("background").cloned().or_else(|| c("color0"))?;
    let fg = wal.special.get("foreground").cloned().or_else(|| c("color7"))?;
    let (bgc, fgc) = (parse_colour(&bg).ok()?, parse_colour(&fg).ok()?);
    let mix = |t: f32| {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        format!("#{:02x}{:02x}{:02x}", m(bgc.r(), fgc.r()), m(bgc.g(), fgc.g()), m(bgc.b(), fgc.b()))
    };
    let luminance = 0.2126 * bgc.r() as f32 + 0.7152 * bgc.g() as f32 + 0.0722 * bgc.b() as f32;
    Some(ColorSection {
        dark: Some(luminance < 128.0),
        background: Some(bg.clone()),
        surface: Some(mix(0.05)),
        raised: Some(mix(0.12)),
        sunken: Some(mix(-0.25)),
        hover: Some(mix(0.10)),
        line: Some(mix(0.20)),
        text: Some(fg.clone()),
        strong: Some(fg.clone()),
        weak: c("color8").or(Some(mix(0.55))),
        icon: Some(fg.clone()),
        icon_dim: Some(mix(0.35)),
        track: Some(mix(0.22)),
        knob: Some(fg.clone()),
        accent: c("color4"),
        playing: c("color6"),
        selection: c("color4"),
        selection_text: Some(bg),
        stars: c("color3"),
        stars_empty: Some(mix(0.22)),
        link: c("color6"),
        warning: c("color3"),
        error: c("color1"),
        success: c("color2"),
    })
}

/// Read yerg.toml, its theme and imports, and resolve them.
pub fn load() -> Loaded {
    let mut errors = Vec::new();
    if let Err(e) = ensure_files() {
        errors.push(format!("cannot set up {}: {e}", config_dir().display()));
    }
    let mut watch = vec![config_path(), themes_dir()];
    let cfg = read_config(&mut errors);

    // Built-in default first, then the theme, imports, and yerg.toml itself.
    let base: ThemeFile = toml::from_str(THEME_DEFAULT).expect("built-in theme parses");
    let mut colours = base.colors.clone();
    let mut font = FontSection::default();
    let mut icons = IconSection::default();
    let mut name = "Default".to_string();

    let theme_name = cfg.theme.clone().unwrap_or_else(|| "default".into());
    if theme_name == "wal" {
        name = "pywal".into();
        if let Some(p) = wal_path() {
            watch.push(p.clone());
            if let Some(c) = wal_theme(&p, &mut errors) {
                colours.overlay(&c);
            }
        }
    } else {
        let path = themes_dir().join(format!("{theme_name}.toml"));
        watch.push(path.clone());
        let file: Option<ThemeFile> = if path.is_file() {
            read_toml(&path, &mut errors)
        } else if let Some((_, body)) = BUNDLED_THEMES.iter().find(|(n, _)| *n == theme_name) {
            parse_toml(body, &theme_name, &mut errors)
        } else {
            errors.push(format!(
                "theme \"{theme_name}\" not found: there is no {} (available: {})",
                path.display(),
                available_themes().join(", ")
            ));
            None
        };
        if let Some(f) = file {
            name = f.name.clone().unwrap_or_else(|| theme_name.clone());
            colours.overlay(&f.colors);
            font.overlay(&f.font);
            icons.overlay(&f.icons);
        }
    }
    for imp in &cfg.import {
        let path = resolve_path(imp);
        watch.push(path.clone());
        if let Some(f) = read_toml::<ThemeFile>(&path, &mut errors) {
            colours.overlay(&f.colors);
            font.overlay(&f.font);
            icons.overlay(&f.icons);
        }
    }
    colours.overlay(&cfg.colors);
    font.overlay(&cfg.font);
    icons.overlay(&cfg.icons);

    let colors = Colors::resolve(&fallback_colours(), &colours, &mut errors, "colors");
    let size = |v: Option<f32>, d: f32, what: &str, errors: &mut Vec<String>| match v {
        Some(x) if (4.0..=96.0).contains(&x) => x,
        Some(x) => {
            errors.push(format!("font.{what} = {x} is out of range (4 to 96)"));
            d
        }
        None => d,
    };
    let body_size = size(font.size, 13.0, "size", &mut errors);
    let small_size = size(font.small, (body_size - 2.0).max(6.0), "small", &mut errors);
    let heading_size = size(font.heading, body_size + 7.0, "heading", &mut errors);
    let mono_size = size(font.mono_size, body_size - 0.5, "mono_size", &mut errors);
    let scale = match font.scale {
        Some(s) if (0.5..=3.0).contains(&s) => s,
        Some(s) => {
            errors.push(format!("font.scale = {s} is out of range (0.5 to 3)"));
            1.0
        }
        None => 1.0,
    };
    let ui_font = load_font(font.family.as_deref(), font.file.as_deref(), "font", &mut errors);
    let mono_font = load_font(font.monospace.as_deref(), font.monospace_file.as_deref(), "font.monospace", &mut errors);
    let icon_font = load_font(icons.font.as_deref(), icons.font_file.as_deref(), "icons.font", &mut errors);

    let icon_scale = match icons.size {
        Some(s) if (0.5..=2.0).contains(&s) => s,
        Some(s) => {
            errors.push(format!("icons.size = {s} is out of range (0.5 to 2)"));
            1.0
        }
        None => 1.0,
    };
    let folder = resolve_path(icons.folder.as_deref().unwrap_or("icons"));
    watch.push(folder.clone());
    let mut icon_files = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(&folder) {
        let mut found: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        // .svg wins over .png when both exist.
        found.sort_by_key(|p| p.extension().map(|e| e.to_ascii_lowercase() != "svg").unwrap_or(true));
        for p in found {
            let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
            if !matches!(ext.as_deref(), Some("svg" | "png" | "jpg" | "jpeg" | "webp")) {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if ICON_NAMES.iter().any(|(n, _)| *n == stem) {
                    icon_files.entry(stem.to_string()).or_insert(p.clone());
                }
            }
        }
    } else if icons.folder.is_some() {
        errors.push(format!("icons.folder: {} is not a folder", folder.display()));
    }
    let mut glyphs = HashMap::new();
    for (k, v) in &icons.glyphs {
        if ICON_NAMES.iter().any(|(n, _)| n == k) {
            glyphs.insert(k.clone(), v.clone());
        } else {
            errors.push(format!("icons.glyphs.{k}: unknown icon (names: {})", ICON_NAMES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")));
        }
    }

    Loaded {
        theme: Theme {
            name,
            colors,
            body_size,
            small_size,
            heading_size,
            mono_size,
            scale,
            ui_font,
            mono_font,
            icon_font,
            icon_scale,
            icon_tint: icons.tint.unwrap_or(true),
            icon_files,
            glyphs,
        },
        errors,
        watch,
    }
}

// ------------------------------------------------------------------ apply

static FONT_KEY: RwLock<String> = RwLock::new(String::new());

/// Push a theme into egui: colours, text sizes, zoom, and fonts (rebuilt
/// only when they changed, as that redoes the glyph atlas).
pub fn apply(ctx: &Context, t: Theme) {
    let c = &t.colors;
    let mut v = if c.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = c.background;
    v.window_fill = c.surface;
    v.faint_bg_color = c.surface;
    v.extreme_bg_color = c.sunken;
    v.selection.bg_fill = c.selection;
    v.selection.stroke = egui::Stroke::new(1.0_f32, c.selection_text);
    v.hyperlink_color = c.link;
    v.warn_fg_color = c.warning;
    v.error_fg_color = c.error;
    v.weak_text_color = Some(c.weak);
    v.widgets.noninteractive.fg_stroke.color = c.text;
    v.widgets.noninteractive.bg_stroke.color = c.line;
    v.widgets.noninteractive.bg_fill = c.background;
    v.widgets.noninteractive.weak_bg_fill = c.background;
    v.widgets.inactive.fg_stroke.color = c.text;
    v.widgets.inactive.weak_bg_fill = c.raised;
    v.widgets.inactive.bg_fill = c.raised;
    v.widgets.hovered.bg_fill = c.hover;
    v.widgets.hovered.weak_bg_fill = c.hover;
    v.widgets.hovered.fg_stroke.color = c.strong;
    v.widgets.active.fg_stroke.color = c.strong;
    v.widgets.open.weak_bg_fill = c.hover;
    let egui_theme = if c.dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(egui_theme);
    ctx.set_visuals_of(egui_theme, v);

    ctx.all_styles_mut(|style| {
        style.text_styles.insert(TextStyle::Small, FontId::new(t.small_size, FontFamily::Proportional));
        style.text_styles.insert(TextStyle::Body, FontId::new(t.body_size, FontFamily::Proportional));
        style.text_styles.insert(TextStyle::Button, FontId::new(t.body_size, FontFamily::Proportional));
        style.text_styles.insert(TextStyle::Heading, FontId::new(t.heading_size, FontFamily::Proportional));
        style.text_styles.insert(TextStyle::Monospace, FontId::new(t.mono_size, FontFamily::Monospace));
    });
    if (ctx.zoom_factor() - t.scale).abs() > f32::EPSILON {
        ctx.set_zoom_factor(t.scale);
    }

    let key = [&t.ui_font, &t.mono_font, &t.icon_font]
        .iter()
        .map(|f| f.as_ref().map(|f| format!("{}#{}:{}", f.label, f.index, f.data.len())).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("|");
    let mut last = FONT_KEY.write().unwrap_or_else(|e| e.into_inner());
    if *last != key || last.is_empty() {
        let mut defs = FontDefinitions::default();
        let add = |name: &str, f: &LoadedFont, defs: &mut FontDefinitions| {
            let mut data = FontData::from_owned(f.data.as_ref().clone());
            data.index = f.index;
            defs.font_data.insert(name.to_string(), Arc::new(data));
        };
        // Custom fonts go first; egui's own stay behind them for emoji and
        // symbols the custom font lacks.
        if let Some(f) = &t.ui_font {
            add("yerg-ui", f, &mut defs);
            defs.families.entry(FontFamily::Proportional).or_default().insert(0, "yerg-ui".into());
        }
        if let Some(f) = &t.mono_font {
            add("yerg-mono", f, &mut defs);
            defs.families.entry(FontFamily::Monospace).or_default().insert(0, "yerg-mono".into());
        }
        let mut icon_family = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
        if let Some(f) = &t.icon_font {
            add("yerg-icon-font", f, &mut defs);
            icon_family.insert(0, "yerg-icon-font".into());
        }
        defs.families.insert(FontFamily::Name(ICON_FAMILY.into()), icon_family);
        ctx.set_fonts(defs);
        *last = key;
    }
    drop(last);
    set_current(t);
}

// ---------------------------------------------------------------- watching

static CHANGED: AtomicBool = AtomicBool::new(false);
static WATCHED: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());

/// Whether the dotfiles changed since the last call.
pub fn take_change() -> bool {
    CHANGED.swap(false, Ordering::Relaxed)
}

/// Replace the set of files the watcher looks at.
pub fn watch(paths: Vec<PathBuf>) {
    *WATCHED.write().unwrap_or_else(|e| e.into_inner()) = paths;
}

fn stamp(paths: &[PathBuf]) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
    let mut out = Vec::new();
    for p in paths {
        let meta = std::fs::metadata(p).ok();
        out.push((p.clone(), meta.as_ref().and_then(|m| m.modified().ok()), meta.as_ref().map(|m| m.len()).unwrap_or(0)));
        // Folders: also every file in them (themes/, icons/).
        if meta.map(|m| m.is_dir()).unwrap_or(false) {
            if let Ok(rd) = std::fs::read_dir(p) {
                let mut entries: Vec<_> = rd
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        let m = e.metadata().ok();
                        (e.path(), m.as_ref().and_then(|m| m.modified().ok()), m.map(|m| m.len()).unwrap_or(0))
                    })
                    .collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                out.extend(entries);
            }
        }
    }
    out
}

/// Poll the watched files twice a second and wake the UI when one changes.
pub fn start_watcher(ctx: Context) {
    std::thread::Builder::new()
        .name("yerg-theme-watch".into())
        .spawn(move || {
            let mut last = stamp(&WATCHED.read().unwrap_or_else(|e| e.into_inner()));
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let now = stamp(&WATCHED.read().unwrap_or_else(|e| e.into_inner()));
                if now != last {
                    last = now;
                    CHANGED.store(true, Ordering::Relaxed);
                    ctx.request_repaint();
                }
            }
        })
        .expect("spawn theme watcher");
}

// ------------------------------------------------------------------ icons

/// Draw a user-supplied icon (glyph or image) for `name` in `rect`, if the
/// theme has one.  Returns false to let the caller draw the built-in one.
pub fn draw_custom_icon(ui: &egui::Ui, name: &str, rect: egui::Rect, colour: Color32) -> bool {
    let t = current();
    if let Some(g) = t.glyphs.get(name) {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            g,
            FontId::new(rect.height() * 0.9, FontFamily::Name(ICON_FAMILY.into())),
            colour,
        );
        return true;
    }
    if let Some(p) = t.icon_files.get(name) {
        let uri = format!("file://{}", p.display());
        let mut img = egui::Image::new(uri).fit_to_exact_size(rect.size());
        if t.icon_tint {
            img = img.tint(colour);
        }
        img.paint_at(ui, rect);
        return true;
    }
    false
}

/// A glyph replacement for a text icon (stars, the playing marker).
pub fn glyph(name: &str) -> Option<String> {
    current().glyphs.get(name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colours() {
        assert_eq!(parse_colour("#18191c").unwrap(), Color32::from_rgb(0x18, 0x19, 0x1c));
        assert_eq!(parse_colour("fff").unwrap(), Color32::WHITE);
        assert!(parse_colour("#12345").is_err());
        assert!(parse_colour("#gggggg").is_err());
        assert!(parse_colour("#ffé").is_err());
    }

    #[test]
    fn bundled_themes_are_complete() {
        // Every bundled theme sets every role, so none relies on the fallback.
        for (name, body) in BUNDLED_THEMES {
            let f: ThemeFile = toml::from_str(body).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut errors = Vec::new();
            let c = Colors::resolve(&fallback_colours(), &f.colors, &mut errors, name);
            assert!(errors.is_empty(), "{name}: {errors:?}");
            let text = body.replace(' ', "");
            for role in ["background=", "text=", "weak=", "accent=", "selection=", "success="] {
                assert!(text.contains(role), "{name} is missing {role}");
            }
            assert!(c.dark != (*name == "light"));
        }
        // And the commented default config parses as is.
        let cfg: ConfigFile = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("default"));
    }

    #[test]
    fn layers_apply_in_order() {
        let mut c = ColorSection { accent: Some("#111111".into()), text: Some("#222222".into()), ..Default::default() };
        c.overlay(&ColorSection { accent: Some("#333333".into()), ..Default::default() });
        assert_eq!(c.accent.as_deref(), Some("#333333"));
        assert_eq!(c.text.as_deref(), Some("#222222"));
        // A typo is reported with a suggestion, and only that line is lost.
        let mut errors = Vec::new();
        let cfg: ConfigFile = parse_toml(
            "theme = \"nord\"\n[colors]\naccnet = \"#fff\"\ntext = \"#eee\"\n[font]\nsiez = 3\n",
            "yerg.toml",
            &mut errors,
        )
        .expect("the rest still parses");
        assert_eq!(cfg.theme.as_deref(), Some("nord"));
        assert_eq!(cfg.colors.text.as_deref(), Some("#eee"));
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("colors.accnet") && errors[0].contains("did you mean \"accent\""), "{errors:?}");
        assert!(errors[1].contains("font.siez") && errors[1].contains("\"size\""), "{errors:?}");
        // A wrong type rejects the file, but the theme choice survives.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.toml"), "theme = \"gruvbox\"\n[font]\nsize = \"big\"\n").unwrap();
        let mut errors = Vec::new();
        assert!(read_toml::<ConfigFile>(&dir.path().join("x.toml"), &mut errors).is_none());
        assert!(errors[0].contains("size"), "{errors:?}");
        assert_eq!(closest("bakground", ROLE_NAMES), Some("background"));
        assert_eq!(closest("zzz", ROLE_NAMES), None);
    }

    #[test]
    fn whole_setup_from_a_config_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("YERG_CONFIG_DIR", dir.path());
        // First run writes the defaults.
        let first = load();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert!(dir.path().join("yerg.toml").is_file());
        assert!(dir.path().join("themes").join("nord.toml").is_file());
        assert_eq!(first.theme.name, "Default");
        // Switching theme keeps the file's comments.
        select_theme("nord").unwrap();
        let text = std::fs::read_to_string(dir.path().join("yerg.toml")).unwrap();
        assert!(text.contains("theme = \"nord\""));
        assert!(text.contains("# Yerg Library appearance."));
        // yerg.toml overrides win over the theme; an icon file is picked up.
        std::fs::write(
            dir.path().join("yerg.toml"),
            "theme = \"nord\"\n[colors]\naccent = \"#ff0000\"\n[font]\nsize = 15\nscale = 1.25\n[icons]\nsize = 1.5\n[icons.glyphs]\nplay = \"P\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("icons").join("queue.svg"), "<svg/>").unwrap();
        let t = load();
        assert!(t.errors.is_empty(), "{:?}", t.errors);
        assert_eq!(t.theme.name, "Nord");
        assert_eq!(t.theme.colors.accent, Color32::from_rgb(255, 0, 0));
        assert_eq!(t.theme.colors.background, parse_colour("#2e3440").unwrap());
        assert_eq!((t.theme.body_size, t.theme.small_size, t.theme.scale, t.theme.icon_scale), (15.0, 13.0, 1.25, 1.5));
        assert_eq!(t.theme.glyphs.get("play").map(String::as_str), Some("P"));
        assert!(t.theme.icon_files.contains_key("queue"));
        // Mistakes are reported and the rest still applies.
        std::fs::write(dir.path().join("yerg.toml"), "theme = \"nope\"\n[colors]\ntext = \"blue\"\n").unwrap();
        let bad = load();
        assert_eq!(bad.errors.len(), 2, "{:?}", bad.errors);
        assert!(bad.errors.iter().any(|e| e.contains("nope")));
        assert_eq!(bad.theme.colors.background, Theme::default().colors.background);
        std::env::remove_var("YERG_CONFIG_DIR");
    }

    #[test]
    fn pywal_colours() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("colors.json");
        std::fs::write(
            &p,
            r##"{"special":{"background":"#101010","foreground":"#e0e0e0","cursor":"#e0e0e0"},
                "colors":{"color0":"#101010","color1":"#aa0000","color2":"#00aa00","color3":"#aaaa00","color4":"#0000aa",
                          "color5":"#aa00aa","color6":"#00aaaa","color7":"#e0e0e0","color8":"#555555"}}"##,
        )
        .unwrap();
        let mut errors = Vec::new();
        let c = wal_theme(&p, &mut errors).unwrap();
        assert!(errors.is_empty());
        assert_eq!(c.dark, Some(true));
        assert_eq!(c.accent.as_deref(), Some("#0000aa"));
        assert_eq!(c.text.as_deref(), Some("#e0e0e0"));
        assert_eq!(c.weak.as_deref(), Some("#555555"));
    }
}
