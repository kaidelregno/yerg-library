// Loader for /YERG/INDEX.BIN written by Yerg Library.
//
// Reads the fixed-stride tables straight into RAM (PSRAM via extmem_malloc on
// a Teensy 4.1 when fitted, otherwise the heap) and gives typed access.  See
// docs/DEVICE_FORMAT.md in the yerg-library repository for the format.
//
//   YergIndex idx;
//   if (!idx.load("/YERG/INDEX.BIN")) { /* fall back to scanning the card */ }
//   for (uint16_t a = 0; a < idx.artistCount; a++)
//     Serial.println(idx.str(idx.artists[a].name));
//   const YergTrack &t = idx.tracks[idx.titleOrder[0]];      // first song by title
//   player.play(idx.str(t.path));   // the original file: .mp3, .flac, .m4a, ...
//
// Everything is little-endian and 4-byte aligned, which matches the Cortex-M7,
// so the records can be used in place.
#pragma once

#include <stdint.h>
#include <string.h>
#include <SD.h>

#define YERG_INDEX_MAGIC    "YRGX"
#define YERG_INDEX_VERSION  1

struct __attribute__((packed)) YergHeader {
  char     magic[4];
  uint16_t version;
  uint16_t sectionCount;
  uint32_t flags;
  uint32_t generatedAt;
  uint32_t libraryId;
  uint32_t totalSize;
  uint32_t crc32;
  uint32_t reserved;
};

struct __attribute__((packed)) YergDirEntry {
  char     tag[4];
  uint32_t offset;
  uint32_t count;
  uint16_t stride;
  uint16_t flags;
};

struct __attribute__((packed)) YergTrack {
  uint32_t id;          // stable id, used in LISTENS.LOG
  uint32_t title;       // string offsets into the pool
  uint32_t artist;
  uint32_t album;
  uint32_t path;        // "/MUSIC/Artist/Album/01 Title.flac" (original format)
  uint32_t lengthMs;
  uint16_t albumIdx;
  uint16_t artistIdx;   // album artist
  uint16_t trackNo;
  uint8_t  discNo;
  uint8_t  rating;
  uint16_t year;
  uint16_t flags;       // bit0 compilation, bit1 has lyrics
  uint32_t genre;
};

struct __attribute__((packed)) YergAlbum {
  uint32_t name;
  uint32_t artist;
  uint16_t firstTrack;
  uint16_t trackCount;
  uint16_t year;
  uint16_t artistIdx;
};

struct __attribute__((packed)) YergArtist {
  uint32_t name;
  uint16_t firstAlbum;
  uint16_t albumCount;
  uint16_t firstTrack;
  uint16_t trackCount;
  uint32_t reserved;
};

struct __attribute__((packed)) YergPlaylist {
  uint32_t name;
  uint32_t firstEntry;
  uint32_t entryCount;
  uint32_t totalMs;
};

static_assert(sizeof(YergHeader) == 32, "header size");
static_assert(sizeof(YergDirEntry) == 16, "dir entry size");
static_assert(sizeof(YergTrack) == 40, "track size");
static_assert(sizeof(YergAlbum) == 16, "album size");
static_assert(sizeof(YergArtist) == 16, "artist size");
static_assert(sizeof(YergPlaylist) == 16, "playlist size");

#if defined(__IMXRT1062__)
extern "C" void *extmem_malloc(size_t);
extern "C" void extmem_free(void *);
static inline void *yergAlloc(size_t n) {
  void *p = extmem_malloc(n);      // PSRAM when fitted
  return p ? p : malloc(n);
}
static inline void yergFree(void *p) {
  if (!p) return;
  // extmem_free tolerates heap pointers on recent Teensyduino; be safe anyway
  if ((uint32_t)p >= 0x70000000u) extmem_free(p); else free(p);
}
#else
static inline void *yergAlloc(size_t n) { return malloc(n); }
static inline void yergFree(void *p) { free(p); }
#endif

class YergIndex {
public:
  const YergTrack    *tracks     = nullptr;  uint32_t trackCount    = 0;
  const YergAlbum    *albums     = nullptr;  uint32_t albumCount    = 0;
  const YergArtist   *artists    = nullptr;  uint32_t artistCount   = 0;
  const YergPlaylist *playlists  = nullptr;  uint32_t playlistCount = 0;
  const uint16_t     *playlistEntries = nullptr; uint32_t playlistEntryCount = 0;
  const uint16_t     *titleOrder = nullptr;  // trackCount entries
  const uint16_t     *albumOrder = nullptr;  // albumCount entries
  const uint16_t     *jumpTitles = nullptr;  // 27 entries: '#', 'A'..'Z'
  const uint16_t     *jumpArtists = nullptr;
  const uint16_t     *jumpAlbums = nullptr;
  uint32_t generatedAt = 0, libraryId = 0;

  const char *str(uint32_t off) const {
    return (off < poolSize_) ? pool_ + off : "";
  }
  const char *trackTitle(uint32_t i) const  { return str(tracks[i].title); }
  const char *trackArtist(uint32_t i) const { return str(tracks[i].artist); }
  const char *trackAlbum(uint32_t i) const  { return str(tracks[i].album); }
  const char *trackPath(uint32_t i) const   { return str(tracks[i].path); }

  // Find a track by its stable id (linear scan; cache if you need it often).
  int findTrack(uint32_t id) const {
    for (uint32_t i = 0; i < trackCount; i++) if (tracks[i].id == id) return (int)i;
    return -1;
  }

  bool loaded() const { return tracks != nullptr; }

  void unload() {
    for (int i = 0; i < bufCount_; i++) yergFree(bufs_[i]);
    bufCount_ = 0;
    tracks = nullptr; albums = nullptr; artists = nullptr; playlists = nullptr;
    playlistEntries = nullptr; titleOrder = nullptr; albumOrder = nullptr;
    jumpTitles = jumpArtists = jumpAlbums = nullptr;
    pool_ = nullptr; poolSize_ = 0;
    trackCount = albumCount = artistCount = playlistCount = playlistEntryCount = 0;
  }

  // Returns false (and loads nothing) if the file is missing, the wrong
  // version, or fails its checksum.
  bool load(const char *path = "/YERG/INDEX.BIN") {
    unload();
    File f = SD.open(path);
    if (!f) return false;
    YergHeader h;
    if (f.read(&h, sizeof h) != sizeof h || memcmp(h.magic, YERG_INDEX_MAGIC, 4) != 0 ||
        h.version != YERG_INDEX_VERSION || h.totalSize != f.size()) {
      f.close();
      return false;
    }
    generatedAt = h.generatedAt;
    libraryId = h.libraryId;

    YergDirEntry dir[16];
    uint16_t n = h.sectionCount < 16 ? h.sectionCount : 16;
    if (f.read(dir, sizeof(YergDirEntry) * n) != (int)(sizeof(YergDirEntry) * n)) {
      f.close();
      return false;
    }
    uint32_t dirEnd = sizeof(YergHeader) + (uint32_t)h.sectionCount * sizeof(YergDirEntry);
    if (!verifyCrc(f, dirEnd, h.totalSize, h.crc32)) {
      f.close();
      return false;
    }

    bool ok = true;
    for (uint16_t i = 0; i < n && ok; i++) {
      const YergDirEntry &e = dir[i];
      size_t bytes = (size_t)e.count * e.stride;
      if ((uint64_t)e.offset + bytes > h.totalSize) { ok = false; break; }
      void *buf = readSection(f, e.offset, bytes);
      if (!buf && bytes) { ok = false; break; }
      if      (!memcmp(e.tag, "TRKS", 4) && e.stride == sizeof(YergTrack))    { tracks = (YergTrack *)buf; trackCount = e.count; }
      else if (!memcmp(e.tag, "ALBS", 4) && e.stride == sizeof(YergAlbum))    { albums = (YergAlbum *)buf; albumCount = e.count; }
      else if (!memcmp(e.tag, "ARTS", 4) && e.stride == sizeof(YergArtist))   { artists = (YergArtist *)buf; artistCount = e.count; }
      else if (!memcmp(e.tag, "PLST", 4) && e.stride == sizeof(YergPlaylist)) { playlists = (YergPlaylist *)buf; playlistCount = e.count; }
      else if (!memcmp(e.tag, "PLEN", 4) && e.stride == 2) { playlistEntries = (uint16_t *)buf; playlistEntryCount = e.count; }
      else if (!memcmp(e.tag, "TTLO", 4) && e.stride == 2) { titleOrder = (uint16_t *)buf; }
      else if (!memcmp(e.tag, "ALBO", 4) && e.stride == 2) { albumOrder = (uint16_t *)buf; }
      else if (!memcmp(e.tag, "JMPT", 4) && e.stride == 2 && e.count == 27) { jumpTitles = (uint16_t *)buf; }
      else if (!memcmp(e.tag, "JMPA", 4) && e.stride == 2 && e.count == 27) { jumpArtists = (uint16_t *)buf; }
      else if (!memcmp(e.tag, "JMPB", 4) && e.stride == 2 && e.count == 27) { jumpAlbums = (uint16_t *)buf; }
      else if (!memcmp(e.tag, "STRS", 4)) { pool_ = (const char *)buf; poolSize_ = e.count; }
      else { yergFree(buf); bufCount_--; }   // unknown section: skip
    }
    f.close();
    if (!ok || !tracks || !pool_ || !albums || !artists) {
      unload();
      return false;
    }
    return true;
  }

private:
  const char *pool_ = nullptr;
  uint32_t poolSize_ = 0;
  void *bufs_[16];
  int bufCount_ = 0;

  void *readSection(File &f, uint32_t offset, size_t bytes) {
    if (bytes == 0) { bufs_[bufCount_++] = nullptr; return nullptr; }
    void *buf = yergAlloc(bytes);
    if (!buf) return nullptr;
    f.seek(offset);
    if ((size_t)f.read(buf, bytes) != bytes) { yergFree(buf); return nullptr; }
    bufs_[bufCount_++] = buf;
    return buf;
  }

  static uint32_t crcTable(uint8_t i) {
    uint32_t c = i;
    for (int k = 0; k < 8; k++) c = (c & 1) ? 0xEDB88320u ^ (c >> 1) : c >> 1;
    return c;
  }

  static bool verifyCrc(File &f, uint32_t from, uint32_t to, uint32_t expected) {
    static uint32_t table[256];
    static bool init = false;
    if (!init) { for (int i = 0; i < 256; i++) table[i] = crcTable(i); init = true; }
    uint8_t buf[512];
    uint32_t crc = 0xFFFFFFFFu;
    f.seek(from);
    uint32_t left = to - from;
    while (left) {
      int n = f.read(buf, left < sizeof buf ? left : sizeof buf);
      if (n <= 0) return false;
      for (int i = 0; i < n; i++) crc = table[(crc ^ buf[i]) & 0xFF] ^ (crc >> 8);
      left -= n;
    }
    return (~crc) == expected;
  }
};

// Append a listen record.  Call when a track stops; the desktop app decides
// whether it counted as a play.  `startedAt` is Unix seconds or 0 when the
// RTC is not set.
static inline void yergLogListen(uint32_t trackId, uint32_t startedAt, uint32_t playedMs) {
  File f = SD.open("/YERG/LISTENS.LOG", FILE_WRITE);
  if (!f) return;
  f.printf("L %08lx %lu %lu %lu\n", (unsigned long)trackId, (unsigned long)startedAt,
           (unsigned long)playedMs, (unsigned long)millis());
  f.close();
}
