//! Device management: registering a card, planning and running a sync,
//! importing the listen log.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use crossbeam_channel::Sender;
use yerg_core::ids;
use yerg_core::model::*;
use yerg_core::plays::{counts_as_play, NewPlay};
use yerg_core::settings::keys;
use yerg_core::{now_unix, Library};

use crate::index::{self, IndexPlaylist, IndexTrack};
use crate::listens;
use crate::names;
use crate::{DEVICE_DIR, DEVICE_FILE, INDEX_FILE, LISTENS_FILE, MUSIC_DIR, PLAYLIST_DIR};

pub const SYNC_MODE_ALL: &str = "all";
pub const SYNC_MODE_PLAYLISTS: &str = "playlists";

/// What `DEVICE.TXT` on the card says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceFile {
    pub id: String,
    pub name: String,
    pub library_id: u32,
    pub synced_at: i64,
    pub index_version: u16,
}

pub fn device_dir(mount: &Path) -> PathBuf {
    mount.join(DEVICE_DIR)
}

pub fn read_device_file(mount: &Path) -> Option<DeviceFile> {
    let text = std::fs::read_to_string(device_dir(mount).join(DEVICE_FILE)).ok()?;
    let mut d = DeviceFile::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "id" => d.id = v.to_string(),
            "name" => d.name = v.to_string(),
            "library" => d.library_id = u32::from_str_radix(v, 16).unwrap_or(0),
            "synced_at" => d.synced_at = v.parse().unwrap_or(0),
            "index_version" => d.index_version = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    if d.id.is_empty() {
        None
    } else {
        Some(d)
    }
}

pub fn write_device_file(mount: &Path, d: &DeviceFile) -> Result<()> {
    let dir = device_dir(mount);
    std::fs::create_dir_all(&dir)?;
    let text = format!(
        "id={}\nname={}\nlibrary={:08x}\nsynced_at={}\nindex_version={}\n",
        d.id,
        names::ascii_fold(&d.name),
        d.library_id,
        d.synced_at,
        d.index_version
    );
    std::fs::write(dir.join(DEVICE_FILE), text)?;
    Ok(())
}

/// A stable id for this library, stamped into the index and device file.
pub fn library_id(lib: &Library) -> u32 {
    let key = "library.id";
    let uuid = match lib.setting(key).ok().flatten() {
        Some(u) => u,
        None => {
            let u = ids::new_uuid();
            let _ = lib.set_setting(key, &u);
            u
        }
    };
    ids::fnv1a32(uuid.as_bytes())
}

/// Register the card at `mount` (or re-attach to one already initialised).
pub fn register_device(lib: &Library, mount: &Path, name: &str) -> Result<Device> {
    if !mount.is_dir() {
        bail!("{} is not a folder", mount.display());
    }
    let existing = read_device_file(mount);
    let id = existing
        .as_ref()
        .map(|d| d.id.clone())
        .unwrap_or_else(|| ids::new_uuid()[..12].to_string());
    let mut device = lib.device(&id)?.unwrap_or(Device {
        id: id.clone(),
        name: name.to_string(),
        mount_path: mount.to_string_lossy().to_string(),
        last_sync: None,
        last_import: None,
        listens_offset: 0,
        sync_mode: SYNC_MODE_ALL.to_string(),
        created_at: now_unix(),
    });
    device.mount_path = mount.to_string_lossy().to_string();
    if !name.trim().is_empty() {
        device.name = name.trim().to_string();
    }
    lib.upsert_device(&device)?;
    write_device_file(
        mount,
        &DeviceFile {
            id: device.id.clone(),
            name: device.name.clone(),
            library_id: library_id(lib),
            synced_at: device.last_sync.unwrap_or(0),
            index_version: index::VERSION,
        },
    )?;
    lib.set_setting(keys::DEVICE_LAST_ID, &device.id)?;
    Ok(device)
}

/// Assign device ids to tracks, resolving the rare FNV collision deterministically.
pub fn assign_device_ids(tracks: &[(TrackId, String)]) -> HashMap<TrackId, u32> {
    let mut sorted: Vec<&(TrackId, String)> = tracks.iter().collect();
    sorted.sort_by(|a, b| a.1.cmp(&b.1));
    let mut used: HashSet<u32> = HashSet::new();
    let mut out = HashMap::with_capacity(tracks.len());
    for (tid, uuid) in sorted {
        let mut id = ids::device_id(uuid);
        let mut n = 0;
        while !used.insert(id) {
            n += 1;
            id = ids::device_id(&format!("{uuid}:{n}"));
        }
        out.insert(*tid, id);
    }
    out
}

/// What a sync would do.
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    /// Tracks that should be on the device after the sync, in library order.
    pub wanted: Vec<Track>,
    /// Tracks to copy (new or changed), with their device paths.
    pub copy: Vec<(Track, String)>,
    /// Device files to remove (no longer selected or source gone).
    pub delete: Vec<DeviceTrack>,
    /// Already up to date.
    pub keep: usize,
    /// Playlists that will be written into the index.
    pub playlists: Vec<Playlist>,
    pub estimated_copy_bytes: u64,
    pub estimated_total_bytes: u64,
    pub device_ids: HashMap<TrackId, u32>,
    pub device_paths: HashMap<TrackId, String>,
}

/// Fingerprint of a track's source file; a change means copy it again.
///
/// Files are copied as they are, so only the file itself matters: tag edits
/// kept in the library reach the player through INDEX.BIN, which is rewritten
/// on every sync.  (`v1` signatures came from WAV transcodes, so cards synced
/// before this change get their tracks replaced by the originals.)
pub fn source_signature(t: &Track) -> String {
    format!("{}:{}:copy-v2", t.mtime, t.file_size)
}

pub fn plan_sync(lib: &Library, device: &Device) -> Result<SyncPlan> {
    let mut plan = SyncPlan::default();
    let all_playlists = lib.playlists()?;
    let mut wanted: Vec<Track> = Vec::new();
    let mut seen: HashSet<TrackId> = HashSet::new();
    if device.sync_mode == SYNC_MODE_PLAYLISTS {
        plan.playlists = all_playlists.iter().filter(|p| p.sync_to_device).cloned().collect();
        for p in &plan.playlists {
            for t in lib.playlist_tracks(p.id)? {
                if !t.missing && seen.insert(t.id) {
                    wanted.push(t);
                }
            }
        }
    } else {
        plan.playlists = all_playlists;
        for t in lib.all_tracks()? {
            if seen.insert(t.id) {
                wanted.push(t);
            }
        }
    }

    let uuids: Vec<(TrackId, String)> = wanted.iter().map(|t| (t.id, t.uuid.clone())).collect();
    plan.device_ids = assign_device_ids(&uuids);

    let on_device: HashMap<TrackId, DeviceTrack> = lib
        .device_tracks(&device.id)?
        .into_iter()
        .map(|d| (d.track_id, d))
        .collect();

    // Keep existing paths stable; only new tracks get fresh names.
    let mut used: HashSet<String> = HashSet::new();
    for t in &wanted {
        if let Some(d) = on_device.get(&t.id) {
            if d.source_sig == source_signature(t) && Path::new(&device.mount_path).join(d.device_path.trim_start_matches('/')).is_file() {
                used.insert(d.device_path.to_ascii_lowercase());
                plan.device_paths.insert(t.id, d.device_path.clone());
                plan.keep += 1;
                plan.estimated_total_bytes += d.size_bytes.max(0) as u64;
            }
        }
    }
    for t in &wanted {
        if plan.device_paths.contains_key(&t.id) {
            continue;
        }
        let path = names::device_path(t, plan.device_ids[&t.id], &mut used);
        let est = t.file_size.max(0) as u64;
        plan.estimated_copy_bytes += est;
        plan.estimated_total_bytes += est;
        plan.device_paths.insert(t.id, path.clone());
        plan.copy.push((t.clone(), path));
    }
    for (tid, d) in &on_device {
        if !seen.contains(tid) || (!plan.device_paths.get(tid).map(|p| p == &d.device_path).unwrap_or(false) && plan.copy.iter().any(|(t, _)| t.id == *tid)) {
            plan.delete.push(d.clone());
        }
    }
    plan.wanted = wanted;
    Ok(plan)
}

#[derive(Debug, Clone)]
pub enum SyncEvent {
    Started { to_copy: usize, to_delete: usize },
    Copying { index: usize, total: usize, title: String },
    Progress { index: usize, fraction: f32 },
    Copied { title: String, bytes: u64 },
    Deleted { path: String },
    Log(String),
    IndexWritten { tracks: usize, albums: usize, artists: usize, playlists: usize, bytes: usize },
    Finished(SyncReport),
}

#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub copied: usize,
    pub deleted: usize,
    pub failed: Vec<String>,
    pub bytes_written: u64,
    pub cancelled: bool,
    pub index_bytes: usize,
}

fn send(events: Option<&Sender<SyncEvent>>, e: SyncEvent) {
    if let Some(tx) = events {
        let _ = tx.send(e);
    }
}

fn card_path(mount: &Path, device_path: &str) -> PathBuf {
    let mut p = mount.to_path_buf();
    for comp in device_path.split('/').filter(|c| !c.is_empty()) {
        p.push(comp);
    }
    p
}

/// Copy one file to the card unchanged.  It is written under a `.part` name
/// and renamed when complete, so a cancelled or failed copy never leaves a
/// truncated track under the real name.  Returns the bytes written.
pub fn copy_to_card(src: &Path, dst: &Path, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<u64> {
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut tmp = dst.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    let result = (|| -> Result<u64> {
        let mut input = std::fs::File::open(src).with_context(|| format!("opening {}", src.display()))?;
        let total = input.metadata().map(|m| m.len()).unwrap_or(0).max(1);
        let mut output = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut written = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            let n = input.read(&mut buf).with_context(|| format!("reading {}", src.display()))?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n]).with_context(|| format!("writing {}", tmp.display()))?;
            written += n as u64;
            progress((written as f64 / total as f64).min(1.0) as f32);
        }
        output.sync_all()?;
        drop(output);
        // Windows will not rename over an existing file.
        if dst.exists() {
            std::fs::remove_file(dst).with_context(|| format!("replacing {}", dst.display()))?;
        }
        std::fs::rename(&tmp, dst).with_context(|| format!("renaming to {}", dst.display()))?;
        Ok(written)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Carry out a plan: delete, copy, write playlists and the index.
pub fn run_sync(
    lib: &Library,
    device: &Device,
    plan: &SyncPlan,
    events: Option<&Sender<SyncEvent>>,
    cancel: &AtomicBool,
) -> Result<SyncReport> {
    let mount = Path::new(&device.mount_path);
    if !mount.is_dir() {
        bail!("device not mounted at {}", mount.display());
    }
    std::fs::create_dir_all(mount.join(MUSIC_DIR))?;
    std::fs::create_dir_all(device_dir(mount))?;
    let mut report = SyncReport::default();
    send(
        events,
        SyncEvent::Started {
            to_copy: plan.copy.len(),
            to_delete: plan.delete.len(),
        },
    );

    // 1. Deletions first to free space.
    for d in &plan.delete {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let p = card_path(mount, &d.device_path);
        match std::fs::remove_file(&p) {
            Err(e) if p.exists() => report.failed.push(format!("delete {}: {e}", d.device_path)),
            _ => {
                lib.delete_device_track(&device.id, d.track_id)?;
                report.deleted += 1;
                send(events, SyncEvent::Deleted { path: d.device_path.clone() });
            }
        }
        remove_empty_parents(&p, &mount.join(MUSIC_DIR));
    }

    // 2. Copy new and changed tracks, in their original format.
    let total = plan.copy.len();
    for (i, (track, dpath)) in plan.copy.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        send(
            events,
            SyncEvent::Copying {
                index: i,
                total,
                title: format!("{} - {}", track.artist, track.display_title()),
            },
        );
        let dst = card_path(mount, dpath);
        let idx = i;
        let res = copy_to_card(Path::new(&track.path), &dst, cancel, |f| {
            send(events, SyncEvent::Progress { index: idx, fraction: f })
        });
        match res {
            Ok(bytes) => {
                lib.upsert_device_track(&DeviceTrack {
                    device_id: device.id.clone(),
                    track_id: track.id,
                    device_path: dpath.clone(),
                    source_sig: source_signature(track),
                    synced_at: now_unix(),
                    size_bytes: bytes as i64,
                })?;
                report.copied += 1;
                report.bytes_written += bytes;
                send(
                    events,
                    SyncEvent::Copied {
                        title: track.display_title().to_string(),
                        bytes,
                    },
                );
            }
            Err(e) => {
                if cancel.load(Ordering::Relaxed) {
                    report.cancelled = true;
                    break;
                }
                let msg = format!("{}: {e:#}", track.path);
                send(events, SyncEvent::Log(format!("failed: {msg}")));
                report.failed.push(msg);
            }
        }
    }

    // 3. Index + playlists reflect what is actually on the card now.
    let (bytes, counts) = write_index(lib, device, &plan.playlists, &plan.device_ids)?;
    report.index_bytes = bytes;
    send(
        events,
        SyncEvent::IndexWritten {
            tracks: counts.0,
            albums: counts.1,
            artists: counts.2,
            playlists: counts.3,
            bytes,
        },
    );

    let mut dev = device.clone();
    dev.last_sync = Some(now_unix());
    lib.upsert_device(&dev)?;
    write_device_file(
        mount,
        &DeviceFile {
            id: dev.id.clone(),
            name: dev.name.clone(),
            library_id: library_id(lib),
            synced_at: dev.last_sync.unwrap_or(0),
            index_version: index::VERSION,
        },
    )?;
    send(events, SyncEvent::Finished(report.clone()));
    Ok(report)
}

fn remove_empty_parents(file: &Path, stop_at: &Path) {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == stop_at || !d.starts_with(stop_at) {
            break;
        }
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// Build INDEX.BIN and the M3U mirrors from the device_tracks table.
/// Returns (index bytes, (tracks, albums, artists, playlists)).
pub fn write_index(
    lib: &Library,
    device: &Device,
    playlists: &[Playlist],
    device_ids: &HashMap<TrackId, u32>,
) -> Result<(usize, (usize, usize, usize, usize))> {
    let mount = Path::new(&device.mount_path);
    let on_device = lib.device_tracks(&device.id)?;
    let track_ids: Vec<TrackId> = on_device.iter().map(|d| d.track_id).collect();
    let tracks = lib.tracks_by_ids(&track_ids)?;
    let by_id: HashMap<TrackId, &Track> = tracks.iter().map(|t| (t.id, t)).collect();

    // Ids for anything not in the plan's map (tracks kept from older syncs)
    let mut ids_map = device_ids.clone();
    let missing: Vec<(TrackId, String)> = tracks
        .iter()
        .filter(|t| !ids_map.contains_key(&t.id))
        .map(|t| (t.id, t.uuid.clone()))
        .collect();
    if !missing.is_empty() {
        // Recompute over the full set so collisions resolve consistently.
        let all: Vec<(TrackId, String)> = tracks.iter().map(|t| (t.id, t.uuid.clone())).collect();
        ids_map = assign_device_ids(&all);
    }

    let mut index_tracks = Vec::with_capacity(on_device.len());
    for d in &on_device {
        let Some(t) = by_id.get(&d.track_id) else { continue };
        index_tracks.push(IndexTrack {
            id: ids_map[&t.id],
            title: t.display_title().to_string(),
            artist: t.artist.clone(),
            album_artist: t.album_artist.clone(),
            album: t.album.clone(),
            genre: t.genre.clone(),
            path: d.device_path.clone(),
            length_ms: t.duration_ms.clamp(0, u32::MAX as i64) as u32,
            track_no: t.track_no.unwrap_or(0).clamp(0, u16::MAX as i32) as u16,
            disc_no: t.disc_no.unwrap_or(0).clamp(0, 255) as u8,
            rating: t.rating.clamp(0, 5) as u8,
            year: t.year.unwrap_or(0).clamp(0, u16::MAX as i32) as u16,
            compilation: t.compilation,
            has_lyrics: t.has_lyrics,
        });
    }
    let path_of: HashMap<TrackId, &str> = on_device.iter().map(|d| (d.track_id, d.device_path.as_str())).collect();

    let pl_dir = device_dir(mount).join(PLAYLIST_DIR);
    let _ = std::fs::remove_dir_all(&pl_dir);
    std::fs::create_dir_all(&pl_dir)?;
    let mut index_playlists = Vec::new();
    let mut used_names = HashSet::new();
    for p in playlists {
        let ids_in = lib.playlist_track_ids(p.id)?;
        let present: Vec<TrackId> = ids_in.into_iter().filter(|id| path_of.contains_key(id)).collect();
        if present.is_empty() {
            continue;
        }
        index_playlists.push(IndexPlaylist {
            name: p.name.clone(),
            track_ids: present.iter().map(|id| ids_map[id]).collect(),
        });
        // M3U mirror (handy for other players and for eyeballing the card)
        let mut fname = names::fat_component(&p.name, 40);
        while !used_names.insert(fname.to_ascii_lowercase()) {
            fname.push('_');
        }
        let mut m3u = String::from("#EXTM3U\n");
        for id in &present {
            let t = by_id[id];
            m3u.push_str(&format!(
                "#EXTINF:{},{} - {}\n..{}\n",
                t.duration_ms / 1000,
                names::ascii_fold(&t.artist),
                names::ascii_fold(t.display_title()),
                path_of[id]
            ));
        }
        std::fs::write(pl_dir.join(format!("{fname}.m3u")), m3u)?;
    }

    let bytes = index::build(&index_tracks, &index_playlists, library_id(lib), now_unix() as u32)?;
    let tmp = device_dir(mount).join("INDEX.TMP");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, device_dir(mount).join(INDEX_FILE))?;
    let parsed = index::read(&bytes)?;
    Ok((
        bytes.len(),
        (parsed.tracks.len(), parsed.albums.len(), parsed.artists.len(), parsed.playlists.len()),
    ))
}

#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub records: usize,
    pub counted: usize,
    pub too_short: usize,
    pub unknown_track: usize,
    pub duplicates: usize,
    pub estimated: usize,
    pub warnings: Vec<String>,
}

/// How device plays enter the scrobble queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceScrobblePolicy {
    /// Queue every imported listen.
    All,
    /// Only listens with a real timestamp.
    TimestampedOnly,
    /// Count them, never scrobble them.
    None,
}

impl DeviceScrobblePolicy {
    pub fn from_setting(s: &str) -> DeviceScrobblePolicy {
        match s {
            "none" => DeviceScrobblePolicy::None,
            "timestamped" => DeviceScrobblePolicy::TimestampedOnly,
            _ => DeviceScrobblePolicy::All,
        }
    }
    pub fn as_setting(self) -> &'static str {
        match self {
            DeviceScrobblePolicy::All => "all",
            DeviceScrobblePolicy::TimestampedOnly => "timestamped",
            DeviceScrobblePolicy::None => "none",
        }
    }
}

/// Read new records from the card's LISTENS.LOG into the play history.
pub fn import_listens(lib: &Library, device: &Device, policy: DeviceScrobblePolicy) -> Result<ImportReport> {
    let mount = Path::new(&device.mount_path);
    let log_path = device_dir(mount).join(LISTENS_FILE);
    let mut report = ImportReport::default();
    if !log_path.is_file() {
        return Ok(report);
    }
    let bytes = std::fs::read(&log_path).with_context(|| format!("reading {}", log_path.display()))?;
    let mut offset = device.listens_offset.max(0) as usize;
    if offset > bytes.len() {
        report.warnings.push("listen log is shorter than last time; re-reading from the start".into());
        offset = 0;
    }
    // Only consume up to the last newline so a half-written record waits for next time.
    let end = bytes[offset..]
        .iter()
        .rposition(|b| *b == b'\n')
        .map(|p| offset + p + 1)
        .unwrap_or(offset);
    if end <= offset {
        return Ok(report);
    }
    let text = String::from_utf8_lossy(&bytes[offset..end]);
    let parsed = listens::parse(&text);
    report.records = parsed.listens.len();
    report.warnings.extend(parsed.warnings);

    // Map device ids -> tracks: what is on the device now, then the whole library.
    let on_device = lib.device_tracks(&device.id)?;
    let dev_tracks = lib.tracks_by_ids(&on_device.iter().map(|d| d.track_id).collect::<Vec<_>>())?;
    let mut id_map: HashMap<u32, Track> = HashMap::new();
    let pairs: Vec<(TrackId, String)> = dev_tracks.iter().map(|t| (t.id, t.uuid.clone())).collect();
    for (tid, did) in assign_device_ids(&pairs) {
        if let Some(t) = dev_tracks.iter().find(|t| t.id == tid) {
            id_map.insert(did, t.clone());
        }
    }
    let needs_fallback = parsed.listens.iter().any(|l| !id_map.contains_key(&l.track_id));
    if needs_fallback {
        for t in lib.all_tracks()? {
            id_map.entry(ids::device_id(&t.uuid)).or_insert(t);
        }
    }

    let now = now_unix();
    let times = listens::assign_timestamps(&parsed.listens, now);
    lib.transaction(|lib| {
        for (l, (started_at, estimated)) in parsed.listens.iter().zip(times) {
            let Some(track) = id_map.get(&l.track_id) else {
                report.unknown_track += 1;
                continue;
            };
            if !counts_as_play(track.duration_ms, l.played_ms) {
                report.too_short += 1;
                continue;
            }
            let scrobble = match policy {
                DeviceScrobblePolicy::All => true,
                DeviceScrobblePolicy::TimestampedOnly => !estimated,
                DeviceScrobblePolicy::None => false,
            };
            let np = NewPlay {
                track_id: track.id,
                started_at,
                played_ms: l.played_ms,
                source: PlaySource::Device,
                device_id: Some(device.id.clone()),
                ts_estimated: estimated,
                scrobble,
            };
            match lib.record_play(&np)? {
                Some(_) => {
                    report.counted += 1;
                    if estimated {
                        report.estimated += 1;
                    }
                }
                None => report.duplicates += 1,
            }
        }
        let mut dev = device.clone();
        dev.listens_offset = end as i64;
        dev.last_import = Some(now);
        lib.upsert_device(&dev)?;
        Ok(())
    })?;
    Ok(report)
}

/// Remove the music and index from a card (not the listen log).
pub fn wipe_device_music(lib: &Library, device: &Device) -> Result<usize> {
    let mount = Path::new(&device.mount_path);
    let tracks = lib.device_tracks(&device.id)?;
    let mut n = 0;
    for d in &tracks {
        let p = card_path(mount, &d.device_path);
        if std::fs::remove_file(&p).is_ok() {
            n += 1;
        }
        remove_empty_parents(&p, &mount.join(MUSIC_DIR));
        lib.delete_device_track(&device.id, d.track_id)?;
    }
    let _ = std::fs::remove_file(device_dir(mount).join(INDEX_FILE));
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ids_are_stable_and_unique() {
        let a = vec![(1, "aaa".to_string()), (2, "bbb".to_string())];
        let m1 = assign_device_ids(&a);
        let m2 = assign_device_ids(&a);
        assert_eq!(m1, m2);
        assert_ne!(m1[&1], m1[&2]);
        assert_eq!(m1[&1], ids::device_id("aaa"));
    }

    #[test]
    fn device_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let d = DeviceFile {
            id: "abc123".into(),
            name: "Yérg".into(),
            library_id: 0xCAFE,
            synced_at: 42,
            index_version: 1,
        };
        write_device_file(dir.path(), &d).unwrap();
        let back = read_device_file(dir.path()).unwrap();
        assert_eq!(back.id, "abc123");
        assert_eq!(back.name, "Yerg");
        assert_eq!(back.library_id, 0xCAFE);
    }

    #[test]
    fn import_counts_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open_in_memory().unwrap();
        let (tid, _) = lib
            .upsert_scanned(
                "/song.mp3",
                &TagData {
                    title: "Song".into(),
                    artist: "A".into(),
                    ..Default::default()
                },
                &AudioProps {
                    duration_ms: 200_000,
                    ..Default::default()
                },
                1,
                1,
                None,
                None,
            )
            .unwrap();
        let device = register_device(&lib, dir.path(), "Test").unwrap();
        let track = lib.track(tid).unwrap().unwrap();
        lib.upsert_device_track(&DeviceTrack {
            device_id: device.id.clone(),
            track_id: tid,
            device_path: "/MUSIC/A/x/01 Song.mp3".into(),
            source_sig: "s".into(),
            synced_at: 0,
            size_bytes: 0,
        })
        .unwrap();
        let did = ids::device_id(&track.uuid);
        let log = format!(
            "# boot 0\nL {did:08x} 0 150000\nL {did:08x} 1700000000 200000\nL {did:08x} 0 5000\nL 00000001 0 150000\n"
        );
        std::fs::write(device_dir(dir.path()).join(LISTENS_FILE), &log).unwrap();
        let r = import_listens(&lib, &device, DeviceScrobblePolicy::TimestampedOnly).unwrap();
        assert_eq!(r.records, 4);
        assert_eq!(r.counted, 2);
        assert_eq!(r.too_short, 1);
        assert_eq!(r.unknown_track, 1);
        assert_eq!(r.estimated, 1);
        assert_eq!(lib.track(tid).unwrap().unwrap().play_count, 2);
        // Only the timestamped listen is queued for scrobbling
        assert_eq!(lib.pending_count(ScrobbleService::LastFm).unwrap(), 1);
        // Second import: nothing new
        let device = lib.device(&device.id).unwrap().unwrap();
        assert!(device.listens_offset > 0);
        let r2 = import_listens(&lib, &device, DeviceScrobblePolicy::All).unwrap();
        assert_eq!(r2.records, 0);
        // Append a record and import again
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(device_dir(dir.path()).join(LISTENS_FILE))
            .unwrap();
        use std::io::Write;
        writeln!(f, "L {did:08x} 1700001000 200000").unwrap();
        let device = lib.device(&device.id).unwrap().unwrap();
        let r3 = import_listens(&lib, &device, DeviceScrobblePolicy::All).unwrap();
        assert_eq!(r3.counted, 1);
        assert_eq!(lib.track(tid).unwrap().unwrap().play_count, 3);
    }

    #[test]
    fn plan_and_index_without_audio() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open_in_memory().unwrap();
        for (i, title) in ["One", "Two"].iter().enumerate() {
            lib.upsert_scanned(
                &format!("/{title}.mp3"),
                &TagData {
                    title: title.to_string(),
                    artist: "Band".into(),
                    album: "LP".into(),
                    track_no: Some(i as i32 + 1),
                    ..Default::default()
                },
                &AudioProps {
                    duration_ms: 100_000,
                    channels: Some(2),
                    ..Default::default()
                },
                3_000_000,
                1,
                None,
                None,
            )
            .unwrap();
        }
        let device = register_device(&lib, dir.path(), "Card").unwrap();
        let plan = plan_sync(&lib, &device).unwrap();
        assert_eq!(plan.copy.len(), 2);
        assert_eq!(plan.copy[0].1, "/MUSIC/Band/LP/01 One.mp3");
        // Copies are the original files, so the estimate is their size.
        assert_eq!(plan.estimated_copy_bytes, 6_000_000);

        // Pretend the copies happened, then write the index.
        for (t, p) in &plan.copy {
            lib.upsert_device_track(&DeviceTrack {
                device_id: device.id.clone(),
                track_id: t.id,
                device_path: p.clone(),
                source_sig: source_signature(t),
                synced_at: 1,
                size_bytes: 10,
            })
            .unwrap();
        }
        let pl = lib.create_playlist("Faves").unwrap();
        lib.add_to_playlist(pl, &[plan.copy[1].0.id]).unwrap();
        let playlists = lib.playlists().unwrap();
        let (bytes, counts) = write_index(&lib, &device, &playlists, &plan.device_ids).unwrap();
        assert!(bytes > 0);
        assert_eq!(counts, (2, 1, 1, 1));
        let idx = index::read(&std::fs::read(device_dir(dir.path()).join(INDEX_FILE)).unwrap()).unwrap();
        assert_eq!(idx.tracks[0].path, "/MUSIC/Band/LP/01 One.mp3");
        assert!(device_dir(dir.path()).join(PLAYLIST_DIR).join("Faves.m3u").is_file());

        // Second plan: everything is kept (paths exist? no files on disk -> re-copy)
        let plan2 = plan_sync(&lib, &device).unwrap();
        assert_eq!(plan2.keep, 0);
        assert_eq!(plan2.copy.len(), 2);
        // Create the files and plan again: now kept
        for (_, p) in &plan.copy {
            let fp = card_path(dir.path(), p);
            std::fs::create_dir_all(fp.parent().unwrap()).unwrap();
            std::fs::write(&fp, b"x").unwrap();
        }
        let plan3 = plan_sync(&lib, &device).unwrap();
        assert_eq!(plan3.keep, 2);
        assert!(plan3.copy.is_empty());
    }

    #[test]
    fn copy_is_byte_identical_and_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("song.flac");
        let data: Vec<u8> = (0..3_500_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let dst = dir.path().join("card").join("MUSIC").join("01 Song.flac");
        let mut fractions = Vec::new();
        let n = copy_to_card(&src, &dst, &AtomicBool::new(false), |f| fractions.push(f)).unwrap();
        assert_eq!(n, data.len() as u64);
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        assert!(!dst.with_file_name("01 Song.flac.part").exists());
        assert_eq!(fractions.last().copied(), Some(1.0));
        assert!(fractions.windows(2).all(|w| w[0] <= w[1]));

        // Copying again replaces the file (Windows cannot rename over it).
        std::fs::write(&src, b"newer").unwrap();
        copy_to_card(&src, &dst, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"newer");
    }

    #[test]
    fn cancelled_copy_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("song.mp3");
        std::fs::write(&src, vec![1u8; 3_000_000]).unwrap();
        let dst = dir.path().join("out").join("song.mp3");
        let cancel = AtomicBool::new(false);
        let err = copy_to_card(&src, &dst, &cancel, |f| {
            if f > 0.3 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("cancelled"));
        assert!(!dst.exists());
        assert!(!dst.with_file_name("song.mp3.part").exists());
    }

    #[test]
    fn sync_copies_originals_and_replaces_old_wavs() {
        let music = tempfile::tempdir().unwrap();
        let card = tempfile::tempdir().unwrap();
        let lib = Library::open_in_memory().unwrap();
        let mut sources = Vec::new();
        for (i, (title, ext)) in [("One", "mp3"), ("Two", "flac"), ("Three", "m4a")].iter().enumerate() {
            let path = music.path().join(format!("{title}.{ext}"));
            let body: Vec<u8> = format!("{title} audio bytes").into_bytes().repeat(1000 * (i + 1));
            std::fs::write(&path, &body).unwrap();
            let (tid, _) = lib
                .upsert_scanned(
                    &path.to_string_lossy(),
                    &TagData {
                        title: title.to_string(),
                        artist: "Band".into(),
                        album: "LP".into(),
                        track_no: Some(i as i32 + 1),
                        ..Default::default()
                    },
                    &AudioProps {
                        duration_ms: 180_000,
                        ..Default::default()
                    },
                    body.len() as i64,
                    1,
                    None,
                    None,
                )
                .unwrap();
            sources.push((tid, body, *ext));
        }
        let device = register_device(&lib, card.path(), "Card").unwrap();

        // A card synced by the old WAV transcoder: track One is there as a WAV.
        let old_wav = card_path(card.path(), "/MUSIC/Band/LP/01 One.wav");
        std::fs::create_dir_all(old_wav.parent().unwrap()).unwrap();
        std::fs::write(&old_wav, b"RIFF....WAVE").unwrap();
        lib.upsert_device_track(&DeviceTrack {
            device_id: device.id.clone(),
            track_id: sources[0].0,
            device_path: "/MUSIC/Band/LP/01 One.wav".into(),
            source_sig: "1:999:00000000:v1".into(),
            synced_at: 1,
            size_bytes: 12,
        })
        .unwrap();

        let plan = plan_sync(&lib, &device).unwrap();
        assert_eq!(plan.copy.len(), 3);
        assert_eq!(plan.delete.len(), 1);
        let report = run_sync(&lib, &device, &plan, None, &AtomicBool::new(false)).unwrap();
        assert_eq!((report.copied, report.deleted), (3, 1));
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(!old_wav.exists());

        let idx = index::read(&std::fs::read(device_dir(card.path()).join(INDEX_FILE)).unwrap()).unwrap();
        for (tid, body, ext) in &sources {
            let dt = lib.device_tracks(&device.id).unwrap().into_iter().find(|d| d.track_id == *tid).unwrap();
            assert!(dt.device_path.ends_with(&format!(".{ext}")), "{}", dt.device_path);
            assert_eq!(&std::fs::read(card_path(card.path(), &dt.device_path)).unwrap(), body);
            assert_eq!(dt.size_bytes, body.len() as i64);
            assert!(idx.tracks.iter().any(|t| t.path == dt.device_path));
        }

        // Nothing changed: the next sync copies nothing.
        let device = lib.device(&device.id).unwrap().unwrap();
        let again = plan_sync(&lib, &device).unwrap();
        assert_eq!((again.keep, again.copy.len(), again.delete.len()), (3, 0, 0));
    }
}
