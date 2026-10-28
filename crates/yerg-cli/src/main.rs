//! `yerg`: command line access to the library, the scrobbler and the device sync.
//! Handy for scripting and for checking the device index without the GUI.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use yerg_core::model::*;
use yerg_core::query::{ArtistMode, Sort, SortKey, TrackFilter, AlbumSort};
use yerg_core::scan::ScanEvent;
use yerg_core::{fmt_bytes, fmt_duration, fmt_total_time, Library};
use yerg_scrobble::lastfm::{Network, LIBREFM_API_KEY, LIBREFM_API_SECRET};
use yerg_scrobble::worker::{flush_pending, ScrobbleConfig};
use yerg_sync::device::{self, DeviceScrobblePolicy, SyncEvent};

#[derive(Parser)]
#[command(name = "yerg", version, about = "Yerg Library command line")]
struct Cli {
    /// Library database (default: the per-user library)
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage library folders
    Folders {
        #[command(subcommand)]
        cmd: FolderCmd,
    },
    /// Scan library folders for changes
    Scan {
        /// Re-read every file even if unchanged
        #[arg(long)]
        full: bool,
    },
    /// List tracks
    Tracks {
        #[arg(long, default_value = "")]
        search: String,
        #[arg(long, value_enum, default_value = "album-artist")]
        sort: SortArg,
        #[arg(long)]
        desc: bool,
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// List albums
    Albums {
        #[arg(long, default_value = "")]
        search: String,
    },
    /// List artists
    Artists {
        /// Group by album artist instead of track artist
        #[arg(long)]
        album_artist: bool,
    },
    /// List genres
    Genres,
    /// Library statistics
    Stats,
    /// Playlists
    Playlist {
        #[command(subcommand)]
        cmd: PlaylistCmd,
    },
    /// Play history
    Plays {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Scrobbling
    Scrobble {
        #[command(subcommand)]
        cmd: ScrobbleCmd,
    },
    /// The MP3 player
    Device {
        #[command(subcommand)]
        cmd: DeviceCmd,
    },
    /// Device index files
    Index {
        #[command(subcommand)]
        cmd: IndexCmd,
    },
    /// Show the tags lofty reads from a file
    Tags { file: PathBuf },
}

#[derive(Subcommand)]
enum FolderCmd {
    List,
    Add { path: PathBuf },
    Remove { id: i64 },
}

#[derive(Subcommand)]
enum PlaylistCmd {
    List,
    Create { name: String },
    Show { id: i64 },
    Add { id: i64, track_ids: Vec<i64> },
    Delete { id: i64 },
    Export { id: i64, file: PathBuf },
    Import {
        file: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Include (or exclude) a playlist in device sync
    Sync {
        id: i64,
        #[arg(long)]
        off: bool,
    },
}

#[derive(Subcommand)]
enum ScrobbleCmd {
    Status,
    /// Submit everything pending now
    Flush,
    /// Authorise Last.fm (needs api key and secret saved first)
    LastfmAuth {
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        api_secret: Option<String>,
    },
    /// Connect Libre.fm (no API account needed: approve the app in the browser)
    LibrefmAuth,
    /// Check that the saved Last.fm API key and secret are accepted (no sign-in)
    LastfmCheck,
    /// Save and validate a ListenBrainz token
    Listenbrainz { token: String },
    /// Mark all pending listens for a service as skipped
    SkipPending { service: ServiceArg },
    /// Put skipped (and optionally ignored) listens back in the queue
    Requeue {
        service: ServiceArg,
        #[arg(long)]
        ignored: bool,
    },
}

#[derive(Subcommand)]
enum DeviceCmd {
    List,
    /// Register the card mounted at a folder
    Add {
        mount: PathBuf,
        #[arg(long, default_value = "Yerg")]
        name: String,
    },
    /// Set sync mode
    Mode { id: String, mode: ModeArg },
    /// Show what a sync would do
    Plan { id: String },
    /// Copy new and changed tracks and write the index
    Sync { id: String },
    /// Import LISTENS.LOG into play history
    Import {
        id: String,
        #[arg(long, value_enum, default_value = "all")]
        policy: PolicyArg,
    },
    /// Remove all music and the index from the card
    Wipe { id: String },
    Forget { id: String },
}

#[derive(Subcommand)]
enum IndexCmd {
    /// Build an index from the whole library (paths as they would be on the card)
    Build { out: PathBuf },
    /// Print the contents of an index file
    Dump {
        file: PathBuf,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum SortArg {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Year,
    Duration,
    Genre,
    PlayCount,
    LastPlayed,
    AddedAt,
    Rating,
    Path,
}

impl From<SortArg> for SortKey {
    fn from(s: SortArg) -> SortKey {
        match s {
            SortArg::Title => SortKey::Title,
            SortArg::Artist => SortKey::Artist,
            SortArg::AlbumArtist => SortKey::AlbumArtist,
            SortArg::Album => SortKey::Album,
            SortArg::Year => SortKey::Year,
            SortArg::Duration => SortKey::Duration,
            SortArg::Genre => SortKey::Genre,
            SortArg::PlayCount => SortKey::PlayCount,
            SortArg::LastPlayed => SortKey::LastPlayed,
            SortArg::AddedAt => SortKey::AddedAt,
            SortArg::Rating => SortKey::Rating,
            SortArg::Path => SortKey::Path,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ServiceArg {
    Lastfm,
    Listenbrainz,
    Librefm,
}

impl From<ServiceArg> for ScrobbleService {
    fn from(s: ServiceArg) -> Self {
        match s {
            ServiceArg::Lastfm => ScrobbleService::LastFm,
            ServiceArg::Listenbrainz => ScrobbleService::ListenBrainz,
            ServiceArg::Librefm => ScrobbleService::LibreFm,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    All,
    Playlists,
}

#[derive(Clone, Copy, ValueEnum)]
enum PolicyArg {
    All,
    Timestamped,
    None,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let lib = match &cli.db {
        Some(p) => Library::open(p)?,
        None => Library::open_default()?,
    };
    match cli.cmd {
        Cmd::Folders { cmd } => match cmd {
            FolderCmd::List => {
                for f in lib.folders()? {
                    println!("{:>4}  {}", f.id, f.path);
                }
            }
            FolderCmd::Add { path } => {
                let abs = std::fs::canonicalize(&path).unwrap_or(path);
                let id = lib.add_folder(&abs)?;
                println!("added folder {id}: {}", yerg_core::db::normalize_path(&abs));
            }
            FolderCmd::Remove { id } => {
                let n = lib.remove_folder(id)?;
                println!("removed folder {id} and {n} tracks");
            }
        },
        Cmd::Scan { full } => {
            let (tx, rx) = crossbeam_channel::unbounded();
            let handle = std::thread::spawn(move || {
                for ev in rx {
                    match ev {
                        ScanEvent::Started { total } => eprintln!("reading {total} files..."),
                        ScanEvent::Progress { done, total, .. } if done % 100 == 0 || done == total => {
                            eprintln!("  {done}/{total}")
                        }
                        _ => {}
                    }
                }
            });
            let summary = yerg_core::scan::scan_library(&lib, Some(&tx), full)?;
            drop(tx);
            let _ = handle.join();
            println!("{summary}");
            for e in summary.errors.iter().take(20) {
                println!("  ! {e}");
            }
        }
        Cmd::Tracks { search, sort, desc, limit } => {
            let filter = TrackFilter {
                search,
                ..Default::default()
            };
            let key: SortKey = sort.into();
            let tracks = lib.query_tracks(&filter, Sort { key, descending: desc })?;
            println!("{:>6}  {:<40} {:<28} {:<28} {:>6} {:>5}", "id", "title", "artist", "album", "time", "plays");
            for t in tracks.iter().take(limit) {
                println!(
                    "{:>6}  {:<40} {:<28} {:<28} {:>6} {:>5}",
                    t.id,
                    clip(t.display_title(), 40),
                    clip(&t.artist, 28),
                    clip(&t.album, 28),
                    fmt_duration(t.duration_ms),
                    t.play_count
                );
            }
            if tracks.len() > limit {
                println!("... {} more", tracks.len() - limit);
            }
        }
        Cmd::Albums { search } => {
            let albums = lib.albums(
                &TrackFilter {
                    search,
                    ..Default::default()
                },
                AlbumSort::Artist,
            )?;
            for a in &albums {
                println!(
                    "{:<36} {:<30} {:>4}  {:>3} tracks  {}",
                    clip(&a.album, 36),
                    clip(&a.album_artist, 30),
                    a.year.map(|y| y.to_string()).unwrap_or_default(),
                    a.track_count,
                    fmt_duration(a.duration_ms)
                );
            }
            println!("{} albums", albums.len());
        }
        Cmd::Artists { album_artist } => {
            let mode = if album_artist { ArtistMode::AlbumArtist } else { ArtistMode::Artist };
            for a in lib.artists(mode, "")? {
                println!("{:<40} {:>3} albums {:>5} tracks {:>6} plays", clip(&a.name, 40), a.album_count, a.track_count, a.play_count);
            }
        }
        Cmd::Genres => {
            for g in lib.genres()? {
                println!("{:<30} {:>5} tracks  {}", clip(&g.name, 30), g.track_count, fmt_total_time(g.duration_ms));
            }
        }
        Cmd::Stats => {
            let s = lib.stats()?;
            println!("{} tracks, {} albums, {} artists", s.tracks, s.albums, s.artists);
            println!("{} of audio, {}", fmt_total_time(s.duration_ms), fmt_bytes(s.bytes));
            println!("{} plays recorded", s.plays);
            println!("database: {}", lib.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
        }
        Cmd::Playlist { cmd } => match cmd {
            PlaylistCmd::List => {
                for p in lib.playlists()? {
                    println!("{:>4}  {:<40} {:>4} tracks  {}  {}", p.id, clip(&p.name, 40), p.track_count, fmt_total_time(p.duration_ms), if p.sync_to_device { "[device]" } else { "" });
                }
            }
            PlaylistCmd::Create { name } => println!("created playlist {}", lib.create_playlist(&name)?),
            PlaylistCmd::Show { id } => {
                for (i, t) in lib.playlist_tracks(id)?.iter().enumerate() {
                    println!("{:>4}  {:>6}  {} - {}", i + 1, t.id, t.artist, t.display_title());
                }
            }
            PlaylistCmd::Add { id, track_ids } => {
                lib.add_to_playlist(id, &track_ids)?;
                println!("added {} tracks", track_ids.len());
            }
            PlaylistCmd::Delete { id } => lib.delete_playlist(id)?,
            PlaylistCmd::Export { id, file } => println!("wrote {} entries to {}", lib.export_m3u(id, &file)?, file.display()),
            PlaylistCmd::Import { file, name } => {
                let (id, n, missing) = lib.import_m3u(&file, name.as_deref())?;
                println!("playlist {id}: {n} tracks matched, {} not found", missing.len());
                for m in missing.iter().take(10) {
                    println!("  ? {m}");
                }
            }
            PlaylistCmd::Sync { id, off } => lib.set_playlist_sync(id, !off)?,
        },
        Cmd::Plays { limit } => {
            for p in lib.recent_plays(limit)? {
                println!(
                    "{}  {:<30} {:<24}  {:<7} lastfm:{:<9} lbz:{:<9} librefm:{:<9}{}",
                    chrono::DateTime::from_timestamp(p.play.started_at, 0).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default(),
                    clip(&p.title, 30),
                    clip(&p.artist, 24),
                    p.play.source.as_str(),
                    p.play.lastfm.label(),
                    p.play.listenbrainz.label(),
                    p.play.librefm.label(),
                    if p.play.ts_estimated { " (est.)" } else { "" }
                );
            }
        }
        Cmd::Scrobble { cmd } => scrobble(&lib, cmd)?,
        Cmd::Device { cmd } => device_cmd(&lib, cmd)?,
        Cmd::Index { cmd } => match cmd {
            IndexCmd::Build { out } => {
                let tracks = lib.all_tracks()?;
                let pairs: Vec<(TrackId, String)> = tracks.iter().map(|t| (t.id, t.uuid.clone())).collect();
                let ids = device::assign_device_ids(&pairs);
                let mut used = std::collections::HashSet::new();
                let index_tracks: Vec<yerg_sync::index::IndexTrack> = tracks
                    .iter()
                    .map(|t| yerg_sync::index::IndexTrack {
                        id: ids[&t.id],
                        title: t.display_title().to_string(),
                        artist: t.artist.clone(),
                        album_artist: t.album_artist.clone(),
                        album: t.album.clone(),
                        genre: t.genre.clone(),
                        path: yerg_sync::names::device_path(t, ids[&t.id], &mut used),
                        length_ms: t.duration_ms.max(0) as u32,
                        track_no: t.track_no.unwrap_or(0).max(0) as u16,
                        disc_no: t.disc_no.unwrap_or(0).clamp(0, 255) as u8,
                        rating: t.rating as u8,
                        year: t.year.unwrap_or(0).clamp(0, 65535) as u16,
                        compilation: t.compilation,
                        has_lyrics: t.has_lyrics,
                    })
                    .collect();
                let mut pls = Vec::new();
                for p in lib.playlists()? {
                    pls.push(yerg_sync::index::IndexPlaylist {
                        name: p.name,
                        track_ids: lib.playlist_track_ids(p.id)?.iter().filter_map(|id| ids.get(id).copied()).collect(),
                    });
                }
                let bytes = yerg_sync::index::build(&index_tracks, &pls, device::library_id(&lib), yerg_core::now_unix() as u32)?;
                std::fs::write(&out, &bytes)?;
                println!("wrote {} ({} tracks)", out.display(), index_tracks.len());
            }
            IndexCmd::Dump { file, limit } => {
                let bytes = std::fs::read(&file)?;
                let idx = yerg_sync::index::read(&bytes)?;
                println!("index: {} bytes, {} tracks, {} albums, {} artists, {} playlists, library {:08x}, generated {}",
                    bytes.len(), idx.tracks.len(), idx.albums.len(), idx.artists.len(), idx.playlists.len(), idx.library_id, idx.generated_at);
                for a in &idx.artists {
                    println!("{}  ({} albums, {} tracks)", a.name, a.album_count, a.track_count);
                }
                println!("--- tracks (album order) ---");
                for (i, t) in idx.tracks.iter().take(limit).enumerate() {
                    println!("{:>5} {:08x} {:>2}-{:<3} {:<30} {:<22} {:<22} {:>6}  {}", i, t.id, t.disc_no, t.track_no, clip(&t.title, 30), clip(&t.artist, 22), clip(&t.album, 22), fmt_duration(t.length_ms as i64), t.path);
                }
                for p in &idx.playlists {
                    println!("playlist {:<30} {} entries, {}", p.name, p.entry_count, fmt_total_time(p.total_ms as i64));
                }
            }
        },
        Cmd::Tags { file } => {
            let info = yerg_core::tags::read_file(&file)?;
            println!("{:#?}", info.tags);
            println!("{:#?}", info.props);
            if let Some(p) = info.picture {
                println!("picture: {} ({} bytes)", p.mime, p.data.len());
            }
        }
    }
    Ok(())
}

/// Last.fm-style desktop auth: get a token, open the approval page, wait up to
/// ten minutes for the user to approve.  Returns (session key, username).
fn browser_auth(network: Network, key: &str, secret: &str) -> Result<(String, String)> {
    let token = yerg_scrobble::lastfm::get_token(network, key, secret)?;
    let url = yerg_scrobble::lastfm::auth_url(network, key, &token);
    println!("Approve Yerg Library on this {} page (opening it in your browser):\n  {url}", network.name());
    let _ = open::that(&url);
    println!("Waiting for approval...");
    let started = std::time::Instant::now();
    loop {
        match yerg_scrobble::lastfm::get_session(network, key, secret, &token) {
            Ok(v) => return Ok(v),
            Err(yerg_scrobble::ScrobbleError::AwaitingApproval) if started.elapsed().as_secs() < 600 => {
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn scrobble(lib: &Library, cmd: ScrobbleCmd) -> Result<()> {
    match cmd {
        ScrobbleCmd::Status => {
            let cfg = ScrobbleConfig::load(lib);
            println!("scrobbling: {}", if cfg.enabled { "on" } else { "off" });
            // Waiting counts only for services that are set up.
            for (service, user) in [
                (ScrobbleService::LastFm, &cfg.lastfm_username),
                (ScrobbleService::ListenBrainz, &cfg.listenbrainz_username),
                (ScrobbleService::LibreFm, &cfg.librefm_username),
            ] {
                if cfg.ready(service) {
                    println!("{}: connected as {user}  pending {}", service.label(), lib.pending_count(service)?);
                } else {
                    println!("{}: not connected", service.label());
                }
            }
            // Which pieces are present (never the values themselves).
            let have = |s: &str| if s.trim().is_empty() { "missing" } else { "set" };
            println!(
                "  Last.fm: enabled {}, api key {}, shared secret {}, session key {}",
                if cfg.lastfm_enabled { "yes" } else { "no" },
                have(&cfg.lastfm_api_key),
                have(&cfg.lastfm_api_secret),
                have(&cfg.lastfm_session_key)
            );
            println!(
                "  ListenBrainz: enabled {}, token {}",
                if cfg.listenbrainz_enabled { "yes" } else { "no" },
                have(&cfg.listenbrainz_token)
            );
            println!(
                "  Libre.fm: enabled {}, session key {}",
                if cfg.librefm_enabled { "yes" } else { "no" },
                have(&cfg.librefm_session_key)
            );
        }
        ScrobbleCmd::Flush => {
            let cfg = ScrobbleConfig::load(lib);
            if !cfg.any_ready() {
                bail!("no scrobble service is configured");
            }
            let r = flush_pending(lib, &cfg);
            println!("submitted {}, ignored {}", r.submitted, r.ignored);
            for e in &r.errors {
                println!("  ! {e}");
            }
        }
        ScrobbleCmd::LastfmAuth { api_key, api_secret } => {
            let mut cfg = ScrobbleConfig::load(lib);
            if let Some(k) = api_key {
                cfg.lastfm_api_key = k;
            }
            if let Some(s) = api_secret {
                cfg.lastfm_api_secret = s;
            }
            if cfg.lastfm_api_key.is_empty() || cfg.lastfm_api_secret.is_empty() {
                bail!("pass --api-key and --api-secret (create them at https://www.last.fm/api/account/create)");
            }
            let (sk, user) = browser_auth(Network::LastFm, &cfg.lastfm_api_key, &cfg.lastfm_api_secret)?;
            cfg.lastfm_session_key = sk;
            cfg.lastfm_username = user.clone();
            cfg.lastfm_enabled = true;
            cfg.save(lib)?;
            println!("connected to Last.fm as {user}");
        }
        ScrobbleCmd::LibrefmAuth => {
            let (sk, user) = browser_auth(Network::LibreFm, LIBREFM_API_KEY, LIBREFM_API_SECRET)?;
            let mut cfg = ScrobbleConfig::load(lib);
            cfg.librefm_session_key = sk;
            cfg.librefm_username = user.clone();
            cfg.librefm_enabled = true;
            cfg.save(lib)?;
            println!("connected to Libre.fm as {user}");
        }
        ScrobbleCmd::LastfmCheck => {
            let cfg = ScrobbleConfig::load(lib);
            if cfg.lastfm_api_key.trim().is_empty() || cfg.lastfm_api_secret.trim().is_empty() {
                bail!("no Last.fm API key and secret saved");
            }
            let (key, secret) = (cfg.lastfm_api_key.trim(), cfg.lastfm_api_secret.trim());
            let token = yerg_scrobble::lastfm::get_token(Network::LastFm, key, secret)
                .context("auth.getToken failed: the API key or shared secret is wrong")?;
            println!("auth.getToken: ok (API key and shared secret accepted)");
            match yerg_scrobble::lastfm::get_session(Network::LastFm, key, secret, &token) {
                Err(yerg_scrobble::ScrobbleError::AwaitingApproval) => {
                    println!("auth.getSession: ok (signature valid; Last.fm is waiting for approval, as expected)")
                }
                Ok(_) => println!("auth.getSession: unexpectedly returned a session"),
                Err(e) => bail!("auth.getSession failed: {e}"),
            }
            println!("session key stored: {}", if cfg.lastfm_session_key.is_empty() { "no (connect in Settings > Scrobbling)" } else { "yes" });
        }
        ScrobbleCmd::Listenbrainz { token } => {
            let lb =yerg_scrobble::listenbrainz::ListenBrainz::new(token.clone());
            let user = lb.validate_token()?;
            let mut cfg = ScrobbleConfig::load(lib);
            cfg.listenbrainz_token = token;
            cfg.listenbrainz_username = user.clone();
            cfg.listenbrainz_enabled = true;
            cfg.save(lib)?;
            println!("connected to ListenBrainz as {user}");
        }
        ScrobbleCmd::SkipPending { service } => println!("skipped {}", lib.skip_all_pending(service.into())?),
        ScrobbleCmd::Requeue { service, ignored } => println!("requeued {}", lib.requeue(service.into(), ignored)?),
    }
    Ok(())
}

fn device_cmd(lib: &Library, cmd: DeviceCmd) -> Result<()> {
    let get = |id: &str| -> Result<Device> { lib.device(id)?.with_context(|| format!("no device {id}")) };
    match cmd {
        DeviceCmd::List => {
            for d in lib.devices()? {
                println!(
                    "{}  {:<20} {:<30} mode={} tracks={} last sync {}",
                    d.id,
                    d.name,
                    d.mount_path,
                    d.sync_mode,
                    lib.device_tracks(&d.id)?.len(),
                    d.last_sync.map(|t| t.to_string()).unwrap_or_else(|| "never".into())
                );
            }
        }
        DeviceCmd::Add { mount, name } => {
            let d = device::register_device(lib, &mount, &name)?;
            println!("device {} ({}) at {}", d.id, d.name, d.mount_path);
        }
        DeviceCmd::Mode { id, mode } => {
            let mut d = get(&id)?;
            d.sync_mode = match mode {
                ModeArg::All => device::SYNC_MODE_ALL,
                ModeArg::Playlists => device::SYNC_MODE_PLAYLISTS,
            }
            .to_string();
            lib.upsert_device(&d)?;
        }
        DeviceCmd::Plan { id } => {
            let d = get(&id)?;
            let plan = device::plan_sync(lib, &d)?;
            println!(
                "{} wanted: {} to copy ({}), {} to delete, {} kept; card total about {}",
                plan.wanted.len(),
                plan.copy.len(),
                fmt_bytes(plan.estimated_copy_bytes as i64),
                plan.delete.len(),
                plan.keep,
                fmt_bytes(plan.estimated_total_bytes as i64)
            );
            for (t, p) in plan.copy.iter().take(30) {
                println!("  + {}  <-  {}", p, t.path);
            }
            for d in plan.delete.iter().take(30) {
                println!("  - {}", d.device_path);
            }
        }
        DeviceCmd::Sync { id } => {
            let d = get(&id)?;
            let plan = device::plan_sync(lib, &d)?;
            println!("{} to copy, {} to delete", plan.copy.len(), plan.delete.len());
            let (tx, rx) = crossbeam_channel::unbounded();
            let printer = std::thread::spawn(move || {
                for ev in rx {
                    match ev {
                        SyncEvent::Copying { index, total, title } => eprintln!("[{}/{}] {}", index + 1, total, title),
                        SyncEvent::Deleted { path } => eprintln!("deleted {path}"),
                        SyncEvent::Log(s) => eprintln!("{s}"),
                        SyncEvent::IndexWritten { tracks, albums, artists, playlists, bytes } => {
                            eprintln!("index: {tracks} tracks, {albums} albums, {artists} artists, {playlists} playlists, {bytes} bytes")
                        }
                        _ => {}
                    }
                }
            });
            let cancel = Arc::new(AtomicBool::new(false));
            let report = device::run_sync(lib, &d, &plan, Some(&tx), &cancel)?;
            drop(tx);
            let _ = printer.join();
            println!("copied {}, deleted {}, failed {}, wrote {}", report.copied, report.deleted, report.failed.len(), fmt_bytes(report.bytes_written as i64));
            for f in &report.failed {
                println!("  ! {f}");
            }
        }
        DeviceCmd::Import { id, policy } => {
            let d = get(&id)?;
            let p = match policy {
                PolicyArg::All => DeviceScrobblePolicy::All,
                PolicyArg::Timestamped => DeviceScrobblePolicy::TimestampedOnly,
                PolicyArg::None => DeviceScrobblePolicy::None,
            };
            let r = device::import_listens(lib, &d, p)?;
            println!(
                "{} records: {} counted ({} with estimated times), {} too short, {} unknown tracks, {} duplicates",
                r.records, r.counted, r.estimated, r.too_short, r.unknown_track, r.duplicates
            );
            for w in r.warnings.iter().take(10) {
                println!("  ! {w}");
            }
        }
        DeviceCmd::Wipe { id } => {
            let d = get(&id)?;
            println!("removed {} files", device::wipe_device_music(lib, &d)?);
        }
        DeviceCmd::Forget { id } => lib.delete_device(&id)?,
    }
    Ok(())
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
