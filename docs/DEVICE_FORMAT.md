# Yerg MP3 player card layout

Everything the desktop app writes to the player's SD card, and the one file it
reads back. The player is a Teensy 4.1 with 16 MB of RAM playing music from
the built-in SD slot, so every structure here is designed to be loaded into RAM
as flat arrays and indexed by offset. All multi-byte integers are little-endian.

```
/MUSIC/<Artist>/<Album>/<NN Title>.<ext> the original audio files (see "Audio files")
/YERG/INDEX.BIN                          the library index (see "INDEX.BIN")
/YERG/LISTENS.LOG                        appended by the firmware (see "LISTENS.LOG")
/YERG/DEVICE.TXT                         identifies the card to the app
/YERG/PLAYLISTS/<name>.m3u               human-readable mirrors of the playlists
```

Paths are ASCII only (names are transliterated, e.g. `Björk` → `Bjork`) and
never longer than 127 bytes including the leading slash, matching the
firmware's 128-byte `PATH_LEN`. Characters outside `A-Z a-z 0-9 space - _ . , '
& ! ( ) [ ] + = @ #` become `_`. Names are unique case-insensitively (FAT).

## Audio files

Each track is copied byte for byte from the library, in its original format:
MP3, FLAC, Ogg Vorbis, Opus, M4A/AAC, WAV, AIFF, WavPack or APE. The file keeps
its own extension, lower-cased (`.mp3`, `.flac`, `.m4a`, …), so the firmware
can pick a decoder from the name. A file whose name has no usable extension is
written as `.bin`. The same title in two formats gets two files.

The files keep whatever tags they carry, but the firmware should take titles,
artists and the rest from `INDEX.BIN`. Tag edits made in the library and not
written back to the files only reach the player that way.

A track is copied again only when its source file changes (size or
modification time); the index is rewritten on every sync. Copies are written
as `<name>.part` and renamed once complete, so a cancelled sync never leaves a
truncated track. Cards synced by older versions of the app, which converted
everything to WAV, have those WAVs replaced by the original files on the next
sync.

The on-card size is the size of the library files, about 1 MB per minute for
320 kbps MP3 and 5 to 6 MB per minute for CD-quality FLAC.

## INDEX.BIN

Fixed-stride tables plus a string pool. The firmware reads the header and
directory, then `fread`s each table into a buffer of `count * stride` bytes.
Strings are referenced by byte offset into the pool; offset 0 is always the
empty string. The pool is ASCII (flag bit 0) and NUL-terminated.

### Header (32 bytes)

| off | type     | meaning                                   |
|-----|----------|-------------------------------------------|
| 0   | char[4]  | magic `YRGX`                              |
| 4   | u16      | version, currently 1                      |
| 6   | u16      | number of directory entries               |
| 8   | u32      | flags: bit 0 = strings are pure ASCII     |
| 12  | u32      | generated_at, Unix seconds                |
| 16  | u32      | library id (also in DEVICE.TXT)           |
| 20  | u32      | total file size in bytes                  |
| 24  | u32      | CRC-32 (IEEE) of everything after the directory |
| 28  | u32      | reserved, 0                               |

### Directory (16 bytes per entry, immediately after the header)

| off | type    | meaning                               |
|-----|---------|---------------------------------------|
| 0   | char[4] | section tag                           |
| 4   | u32     | byte offset of the section in the file |
| 8   | u32     | record count (bytes for `STRS`)       |
| 12  | u16     | record stride in bytes                |
| 14  | u16     | flags, 0                              |

Sections are 4-byte aligned. Unknown tags should be skipped so later versions
can add sections.

### Sections

| tag    | stride | record  | order |
|--------|--------|---------|-------|
| `TRKS` | 40     | track   | album artist, album, disc, track number, title |
| `ALBS` | 16     | album   | album artist, year, name (a contiguous run of `TRKS`) |
| `ARTS` | 16     | album artist | name (contiguous runs of `ALBS` and `TRKS`) |
| `PLST` | 16     | playlist | library order |
| `PLEN` | 2      | u16 track index | playlist order, all playlists back to back |
| `TTLO` | 2      | u16 track index | by title (the "Songs" view) |
| `ALBO` | 2      | u16 album index | by album name |
| `JMPT` | 2      | 27 × u16 | position in `TTLO` where `#`, `A` … `Z` start |
| `JMPA` | 2      | 27 × u16 | position in `ARTS` where each letter starts |
| `JMPB` | 2      | 27 × u16 | position in `ALBO` where each letter starts |
| `STRS` | 1      | bytes   | string pool |

Sorting ignores case and a leading "The ", "A " or "An ". A jump-table bucket
with no entries points at the next populated bucket, so `JMP*[b+1] - JMP*[b]`
is always that letter's count.

Track record (`TRKS`, 40 bytes):

| off | type | meaning |
|-----|------|---------|
| 0   | u32  | id: stable 32-bit track id (FNV-1a of the library UUID); never 0; used in LISTENS.LOG |
| 4   | u32  | title (string offset) |
| 8   | u32  | artist (string offset; the track artist, which may differ from the album artist) |
| 12  | u32  | album (string offset) |
| 16  | u32  | path (string offset), absolute, forward slashes |
| 20  | u32  | length in milliseconds |
| 24  | u16  | album index into `ALBS` |
| 26  | u16  | album-artist index into `ARTS` |
| 28  | u16  | track number (0 = unknown) |
| 30  | u8   | disc number (0 = unknown) |
| 31  | u8   | rating 0–5 |
| 32  | u16  | year (0 = unknown) |
| 34  | u16  | flags: bit 0 compilation, bit 1 has lyrics |
| 36  | u32  | genre (string offset) |

Album record (`ALBS`, 16 bytes): `u32 name`, `u32 artist` (string offsets),
`u16 first_track`, `u16 track_count`, `u16 year`, `u16 artist_idx`.

Artist record (`ARTS`, 16 bytes): `u32 name`, `u16 first_album`,
`u16 album_count`, `u16 first_track`, `u16 track_count`, `u32 reserved`.

Playlist record (`PLST`, 16 bytes): `u32 name`, `u32 first_entry`,
`u32 entry_count` (into `PLEN`), `u32 total_ms`.

Memory budget: 40 bytes per track plus strings, so a 10 000-track card is
roughly 400 KB of tables and about 1 MB of strings. See
`firmware/yerg_index.h` for a C loader.

The app writes the index to `INDEX.TMP` and renames it, so the firmware never
sees a half-written file.

## LISTENS.LOG

Plain ASCII, one record per line, appended by the firmware whenever a track
stops (end of track, skip, power-down hook, …):

```
L <track id, 8 hex digits> <started_at, Unix seconds or 0> <played_ms> [<uptime_ms>]
# boot <unix seconds or 0>
```

* `track id` is the `TRKS` id, so it survives re-syncs and renames.
* `started_at` is 0 when the RTC is not set. The app then estimates times:
  runs of unknown-time listens are packed backwards from the next real
  timestamp (or from the import time), spaced by how long each one played.
  Those plays are flagged "estimated" and can be kept out of scrobbling
  (Settings on the player page: All / Only real timestamps / Never).
* Log every stop; the app applies the usual rule (half the track or four
  minutes, tracks longer than 30 s) when importing.
* Lines starting with `#` are ignored (a `# boot` line is handy for debugging).
* The app remembers how many bytes it has imported and only reads complete
  lines, so a line torn by power loss is picked up once the next line arrives.
  If the file shrinks (you deleted it), it is read from the start again;
  duplicates are suppressed by (track, timestamp).

Firmware sketch:

```c
// on track stop
File f = SD.open("/YERG/LISTENS.LOG", FILE_WRITE);   // opens for append
f.printf("L %08lx %lu %lu %lu\n", (unsigned long)track.id,
         (unsigned long)(rtcValid ? startedAt : 0),
         (unsigned long)playedMs, (unsigned long)millis());
f.close();
```

## DEVICE.TXT

`key=value` lines: `id`, `name`, `library` (hex), `synced_at`, `index_version`.
The app uses `id` to recognise a card it has seen before regardless of the
drive letter or mount point; the firmware can show `name`.
