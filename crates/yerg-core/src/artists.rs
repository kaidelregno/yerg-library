//! Splitting artist tags into individual artists, so a collaboration such as
//! "Skrillex, Boys Noize & Dylan Brady" lists under Skrillex, Boys Noize and
//! Dylan Brady instead of being an artist of its own.
//!
//! Separators come in two kinds:
//!
//! * **Clear** ones always split: `feat.`, `ft.`, `featuring` (with or without
//!   brackets), `;`, ` / ` and NUL (ID3v2.4 multi-value tags).
//! * **Ambiguous** ones also appear inside real names: `, `, ` & `, ` and `,
//!   ` x `, ` + `, ` with `, ` vs. ` and `/` without spaces (Tyler, the Creator;
//!   Simon & Garfunkel; Earth, Wind & Fire; AC/DC).  A tag is split on these
//!   only when at least one resulting part is an artist the library already
//!   knows: one that appears on its own somewhere (as an artist, an album
//!   artist, or a part found by a clear split or an accepted split).  Names
//!   listed after `feat.` split on them freely, unless a run of them is a
//!   known name.
//!
//! A part beginning with a lowercase "the " ("the Creator", "the Lizard
//! Wizard") or a suffix such as "Jr." stays attached to the name before it.
//! Parts are compared case-insensitively, so "Charli XCX" and "Charli xcx"
//! are one artist.
//!
//! This follows what other players do (Navidrome, Jellyfin, Poweramp): split
//! on clear separators by default, prefer real multi-value tags, and keep an
//! exception list of names that must never be split.  Here:
//!
//! * A file's ARTISTS tag (one credited artist per value, as MusicBrainz
//!   Picard writes it) is combined with the split ARTIST tag, and every
//!   credited name counts as known.
//! * [`BUILT_IN_EXCEPTIONS`] plus the user's own list are never split.

use std::collections::{HashMap, HashSet};

/// How many times newly found artists are fed back in as known names.
const LEARNING_PASSES: usize = 4;

/// Words that start a list of featured artists, matched case-insensitively
/// after a space or an opening bracket: "A feat. B", "A (ft. B)".
const FEAT_MARKERS: [&str; 5] = ["feat. ", "feat ", "ft. ", "ft ", "featuring "];

/// Ambiguous separators, longest first so ", & " wins over ", ".
/// `true` means matched case-insensitively.
const SOFT_SEPARATORS: [(&str, bool); 11] = [
    (", & ", true),
    (", and ", true),
    (" & ", true),
    (" and ", true),
    (" with ", true),
    (" vs. ", true),
    (" vs ", true),
    (" + ", true),
    (" x ", false), // lowercase only: "Lil Nas X" stays whole
    (", ", true),
    ("/", true),
];

/// Parts that belong to the name before them.
const SUFFIXES: [&str; 6] = ["jr.", "jr", "sr.", "sr", "ii", "iii"];

/// Well-known names that contain separators.  Most are protected anyway
/// (their parts are not artists of their own), but these hold even when,
/// say, a "Fire" turns up in the library.  Users add their own in Settings.
pub const BUILT_IN_EXCEPTIONS: &[&str] = &[
    "AC/DC",
    "Above & Beyond",
    "Bob Marley & the Wailers",
    "Blood, Sweat & Tears",
    "Belle and Sebastian",
    "Brooks & Dunn",
    "Chase & Status",
    "Crosby, Stills & Nash",
    "Crosby, Stills, Nash & Young",
    "Earth, Wind & Fire",
    "Echo & the Bunnymen",
    "Emerson, Lake & Palmer",
    "Florence + the Machine",
    "Hall & Oates",
    "Daryl Hall & John Oates",
    "Hootie & the Blowfish",
    "Ike & Tina Turner",
    "Iron & Wine",
    "Kool & the Gang",
    "Marina and the Diamonds",
    "Mumford & Sons",
    "Peter, Paul and Mary",
    "Prince and the Revolution",
    "Sam & Dave",
    "Simon & Garfunkel",
    "Sly & the Family Stone",
    "Tom Petty and the Heartbreakers",
    "Tyler, the Creator",
];

fn key(s: &str) -> String {
    s.trim().to_lowercase()
}

/// One piece of a chunk and the separator text that came before it.
#[derive(Debug, Clone)]
struct Token {
    sep: String,
    text: String,
}

fn join(tokens: &[Token]) -> String {
    let mut out = String::new();
    for (i, t) in tokens.iter().enumerate() {
        if i > 0 {
            out.push_str(&t.sep);
        }
        out.push_str(&t.text);
    }
    out.trim().to_string()
}

/// Split a tag on clear separators.  Returns (main chunks, featured chunks).
fn clear_split(raw: &str) -> (Vec<String>, Vec<String>) {
    let raw = raw.replace('\0', ";");
    // Featured section: from the first marker on, brackets around it dropped.
    let padded = format!(" {} ", raw.trim());
    let lower = padded.to_ascii_lowercase();
    let mut cut: Option<(usize, usize, bool)> = None;
    for m in FEAT_MARKERS {
        for (prefix, bracketed) in [(" ", false), (" (", true), (" [", true)] {
            let pattern = format!("{prefix}{m}");
            if let Some(p) = lower.find(&pattern) {
                if cut.map(|(c, _, _)| p < c).unwrap_or(true) {
                    cut = Some((p, p + pattern.len(), bracketed));
                }
            }
        }
    }
    let (main, featured) = match cut {
        Some((start, end, bracketed)) => {
            let mut main = padded[..start].trim_end().to_string();
            let mut feat = padded[end..].trim().to_string();
            if !bracketed {
                // nothing to close
            } else if let Some(close) = feat.find([')', ']']) {
                // "A (feat. B) & C": what follows the bracket belongs to the main list.
                let rest = feat[close + 1..].trim().to_string();
                feat.truncate(close);
                if !rest.is_empty() {
                    main = format!("{} {rest}", main.trim_end());
                }
            }
            (main, feat)
        }
        None => (padded.trim().to_string(), String::new()),
    };
    let hard = |s: &str| -> Vec<String> {
        s.split(';')
            .flat_map(|c| c.split(" / "))
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect()
    };
    (hard(&main), hard(&featured))
}

/// Split a chunk into tokens on ambiguous separators, gluing "the …" and
/// suffix parts back onto the token before them.
fn tokenize(chunk: &str) -> Vec<Token> {
    let lower = chunk.to_ascii_lowercase();
    let mut tokens: Vec<Token> = Vec::new();
    let mut start = 0;
    let mut sep_before = String::new();
    let mut i = 0;
    while i < chunk.len() {
        if !chunk.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let mut matched = None;
        for (sep, insensitive) in SOFT_SEPARATORS {
            let hit = if insensitive {
                lower[i..].starts_with(sep)
            } else {
                chunk[i..].starts_with(sep)
            };
            // A separator needs a name on both sides.
            if hit && i > start && i + sep.len() < chunk.len() {
                matched = Some(sep.len());
                break;
            }
        }
        match matched {
            Some(n) => {
                tokens.push(Token {
                    sep: std::mem::take(&mut sep_before),
                    text: chunk[start..i].trim().to_string(),
                });
                sep_before = chunk[i..i + n].to_string();
                i += n;
                start = i;
            }
            None => i += 1,
        }
    }
    tokens.push(Token {
        sep: sep_before,
        text: chunk[start..].trim().to_string(),
    });

    // Glue "the Creator" and "Jr." onto the previous name.
    let mut glued: Vec<Token> = Vec::with_capacity(tokens.len());
    for t in tokens {
        let attach = !glued.is_empty() && (t.text.starts_with("the ") || SUFFIXES.contains(&t.text.to_ascii_lowercase().as_str()));
        if attach {
            let prev = glued.last_mut().expect("checked");
            prev.text = format!("{}{}{}", prev.text, t.sep, t.text);
        } else {
            glued.push(t);
        }
    }
    glued.retain(|t| !t.text.is_empty());
    glued
}

/// Names the library knows as artists in their own right, and names that
/// must never be split.
#[derive(Debug, Default, Clone)]
pub struct KnownArtists {
    keys: HashSet<String>,
    exceptions: HashSet<String>,
}

impl KnownArtists {
    /// Known from the library, or an exception.
    pub fn contains(&self, name: &str) -> bool {
        let k = key(name);
        self.keys.contains(&k) || self.exceptions.contains(&k)
    }

    pub fn is_exception(&self, name: &str) -> bool {
        self.exceptions.contains(&key(name))
    }

    fn insert(&mut self, name: &str) -> bool {
        let k = key(name);
        !k.is_empty() && self.keys.insert(k)
    }

    fn add_exception(&mut self, name: &str) {
        let k = key(name);
        if !k.is_empty() {
            self.exceptions.insert(k);
        }
    }
}

/// Split one chunk (no clear separators left in it).
fn split_chunk(tokens: &[Token], known: &KnownArtists, featured: bool) -> Vec<String> {
    if tokens.len() <= 1 || known.is_exception(&join(tokens)) {
        return vec![join(tokens)];
    }
    // "feat. Simon & Garfunkel": a featured list that is itself a known name
    // gets the strict check instead, which keeps a real name whole but still
    // splits a known collaboration ("feat. Bladee & Ecco2K" when the library
    // also has a "Bladee & ECCO2K" tag).
    if featured && known.contains(&join(tokens)) {
        return split_chunk(tokens, known, false);
    }
    let n = tokens.len();
    // Greedy: at each position take the longest run that is a known name,
    // never the whole chunk (that is the tag being split).
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        let mut end = i + 1;
        for j in (i + 1..=n).rev() {
            if j - i < n && known.contains(&join(&tokens[i..j])) {
                end = j;
                break;
            }
        }
        runs.push((i, end));
        i = end;
    }
    let any_known = runs.iter().any(|(a, b)| known.contains(&join(&tokens[*a..*b])));
    if !(featured || any_known) {
        return vec![join(tokens)];
    }
    // A run of several tokens is a known name, but may itself be a
    // collaboration ("Kanye West & Jay-Z"): check it again, strictly.
    runs.iter()
        .flat_map(|(a, b)| {
            if b - a > 1 {
                split_chunk(&tokens[*a..*b], known, false)
            } else {
                vec![join(&tokens[*a..*b])]
            }
        })
        .collect()
}

/// Split one artist tag into artists, given what the library knows.
pub fn split_artist(raw: &str, known: &KnownArtists) -> Vec<String> {
    let (main, featured) = clear_split(raw);
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let parts = main
        .iter()
        .flat_map(|c| split_chunk(&tokenize(c), known, false))
        .chain(featured.iter().flat_map(|c| split_chunk(&tokenize(c), known, true)));
    for p in parts {
        let p = p.trim().to_string();
        if !p.is_empty() && seen.insert(key(&p)) {
            out.push(p);
        }
    }
    if out.is_empty() && !raw.trim().is_empty() {
        out.push(raw.trim().to_string());
    }
    out
}

/// Split a track's ARTISTS credits: one name per line, though some taggers
/// pack several into one value ("Denzel Curry/Kenny Beats", "A;B").
fn split_credits(credits: &str, known: &KnownArtists) -> Vec<String> {
    credits
        .lines()
        .filter(|l| !l.trim().is_empty())
        .flat_map(|l| split_artist(l, known))
        .collect()
}

/// Every artist tag in the library, split.  Build it from each track's artist
/// tag and ARTISTS credits; album artists count as known names too.
#[derive(Debug, Default, Clone)]
pub struct ArtistIndex {
    known: KnownArtists,
    split: HashMap<String, Vec<String>>,
}

impl ArtistIndex {
    /// `tracks`: (artist tag, credits one per line).  `exceptions`: names
    /// never to split, on top of [`BUILT_IN_EXCEPTIONS`].
    pub fn build<'a>(
        tracks: impl IntoIterator<Item = (&'a str, &'a str)>,
        album_artists: impl IntoIterator<Item = &'a str>,
        exceptions: impl IntoIterator<Item = &'a str>,
    ) -> ArtistIndex {
        let mut known = KnownArtists::default();
        for e in BUILT_IN_EXCEPTIONS.iter().copied().chain(exceptions) {
            known.add_exception(e);
        }
        let mut tags: Vec<&str> = Vec::new();
        let mut seen_tags: HashSet<&str> = HashSet::new();
        let mut credits: Vec<&str> = Vec::new();
        let mut seen_credits: HashSet<&str> = HashSet::new();
        for (artist, credit) in tracks {
            if !artist.trim().is_empty() && seen_tags.insert(artist) {
                tags.push(artist);
            }
            if !credit.trim().is_empty() && seen_credits.insert(credit) {
                credits.push(credit);
            }
        }
        // Credited names are the strongest evidence of who is an artist.
        for c in &credits {
            for line in c.lines() {
                let (main, featured) = clear_split(line);
                for n in main.iter().chain(featured.iter()) {
                    known.insert(n);
                }
            }
        }
        for t in tags.iter().copied().chain(album_artists) {
            known.insert(t);
            // The lead artists of "A feat. B" and the parts of "A; B" are
            // names in their own right.  Featured lists are not added whole,
            // or "feat. X & Y" would never split.
            let (main, _) = clear_split(t);
            for c in &main {
                known.insert(c);
            }
        }
        // Artists found inside collaborations make more collaborations
        // recognisable ("Frost Children, The Deep" teaches "The Deep").
        for _ in 0..LEARNING_PASSES {
            let mut grew = false;
            for t in &tags {
                for p in split_artist(t, &known) {
                    grew |= known.insert(&p);
                }
            }
            for c in &credits {
                for p in split_credits(c, &known) {
                    grew |= known.insert(&p);
                }
            }
            if !grew {
                break;
            }
        }
        let split = tags.iter().map(|t| (t.to_string(), split_artist(t, &known))).collect();
        ArtistIndex { known, split }
    }

    /// The artists in one artist tag.
    pub fn artists_of(&self, raw: &str) -> Vec<String> {
        match self.split.get(raw) {
            Some(v) => v.clone(),
            None => split_artist(raw, &self.known),
        }
    }

    /// A track's artists: its split artist tag, then anyone else its ARTISTS
    /// credits name (credits can be incomplete, and so can the tag).
    pub fn artists_of_track(&self, artist_tag: &str, credits: &str) -> Vec<String> {
        let mut out = self.artists_of(artist_tag);
        if credits.trim().is_empty() {
            return out;
        }
        let mut seen: HashSet<String> = out.iter().map(|a| key(a)).collect();
        for c in split_credits(credits, &self.known) {
            if seen.insert(key(&c)) {
                out.push(c);
            }
        }
        out
    }

    /// Whether a track is by `artist`: one of its artists, or its whole
    /// artist tag (so an old collaboration name still finds its tracks).
    pub fn track_has_artist(&self, artist_tag: &str, credits: &str, artist: &str) -> bool {
        if artist_tag == artist {
            return true;
        }
        let k = key(artist);
        self.artists_of_track(artist_tag, credits).iter().any(|a| key(a) == k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(tags: &[&str]) -> ArtistIndex {
        ArtistIndex::build(tags.iter().map(|t| (*t, "")), std::iter::empty(), std::iter::empty())
    }

    #[test]
    fn names_with_separators_stay_whole() {
        let idx = index(&[
            "Tyler, the Creator",
            "Simon & Garfunkel",
            "Earth, Wind & Fire",
            "Hey, ily!",
            "AC/DC",
            "King Gizzard & the Lizard Wizard",
            "Patrik and the Small Guy",
            "Florence + the Machine",
            "DV&LM",
            "Lil Nas X",
            "Sammy Davis, Jr.",
            "DOMi & JD BECK",
            "Crosby, Stills, Nash & Young",
        ]);
        for t in [
            "Tyler, the Creator",
            "Simon & Garfunkel",
            "Earth, Wind & Fire",
            "Hey, ily!",
            "AC/DC",
            "King Gizzard & the Lizard Wizard",
            "Patrik and the Small Guy",
            "Florence + the Machine",
            "DV&LM",
            "Lil Nas X",
            "Sammy Davis, Jr.",
            "DOMi & JD BECK",
            "Crosby, Stills, Nash & Young",
        ] {
            assert_eq!(idx.artists_of(t), vec![t.to_string()], "{t}");
        }
    }

    #[test]
    fn collaborations_split() {
        let idx = index(&[
            "Skrillex",
            "Boys Noize",
            "Tyler, the Creator",
            "Kali Uchis",
            "Skrillex, Boys Noize & Dylan Brady",
            "Tyler, the Creator, Kali Uchis",
            "Kali Uchis & Tyler, the Creator",
            "Bladee",
            "Bladee, ECCO2K & Thaiboy Digital",
            "DOMi & JD BECK",
            "DOMi & JD BECK/Anderson .Paak",
            "DV&LM",
            "DV&LM, Maddix, Da Hool, & Kiki Solvej",
            "DV&LM & Bassjackers",
        ]);
        assert_eq!(idx.artists_of("Skrillex, Boys Noize & Dylan Brady"), ["Skrillex", "Boys Noize", "Dylan Brady"]);
        assert_eq!(idx.artists_of("Tyler, the Creator, Kali Uchis"), ["Tyler, the Creator", "Kali Uchis"]);
        assert_eq!(idx.artists_of("Kali Uchis & Tyler, the Creator"), ["Kali Uchis", "Tyler, the Creator"]);
        assert_eq!(idx.artists_of("Bladee, ECCO2K & Thaiboy Digital"), ["Bladee", "ECCO2K", "Thaiboy Digital"]);
        // "/" without spaces splits because "DOMi & JD BECK" is known; the duo stays whole.
        assert_eq!(idx.artists_of("DOMi & JD BECK/Anderson .Paak"), ["DOMi & JD BECK", "Anderson .Paak"]);
        // Oxford comma, and "&" inside a name without spaces.
        assert_eq!(idx.artists_of("DV&LM, Maddix, Da Hool, & Kiki Solvej"), ["DV&LM", "Maddix", "Da Hool", "Kiki Solvej"]);
        assert_eq!(idx.artists_of("DV&LM & Bassjackers"), ["DV&LM", "Bassjackers"]);
    }

    #[test]
    fn clear_separators_always_split() {
        let idx = index(&[]);
        let known = KnownArtists::default();
        assert_eq!(split_artist("8485; fish narc", &known), ["8485", "fish narc"]);
        assert_eq!(split_artist("Charli XCX / Hannah Diamond", &known), ["Charli XCX", "Hannah Diamond"]);
        assert_eq!(split_artist("Bladee\0Ecco2k", &known), ["Bladee", "Ecco2k"]);
        assert_eq!(split_artist("Comet;Comet", &known), ["Comet"]);
        assert_eq!(split_artist("deadmau5 feat. Rob Swire", &known), ["deadmau5", "Rob Swire"]);
        assert_eq!(split_artist("Frost Children ft. Kim Petras", &known), ["Frost Children", "Kim Petras"]);
        assert_eq!(split_artist("Artist (feat. Guest)", &known), ["Artist", "Guest"]);
        // Featured lists split on "&" and commas even with nothing known...
        assert_eq!(split_artist("JID feat. 21 Savage & Baby Tate", &known), ["JID", "21 Savage", "Baby Tate"]);
        assert_eq!(
            split_artist("Skrillex, Parisi & Bgirl Feat. Whitearmor", &known),
            ["Skrillex, Parisi & Bgirl", "Whitearmor"]
        );
        assert!(idx.artists_of("").is_empty());
    }

    #[test]
    fn featured_known_names_stay_whole() {
        let idx = index(&["Simon & Garfunkel", "Someone feat. Simon & Garfunkel"]);
        assert_eq!(idx.artists_of("Someone feat. Simon & Garfunkel"), ["Someone", "Simon & Garfunkel"]);
        // A known *collaboration* after feat. still splits (seen in a real library).
        let tracks = [
            ("Bladee", ""),
            ("Ecco2K", ""),
            ("Bladee & ECCO2K", ""),
            ("Varg²™ feat. Bladee & Ecco2K", "Varg²™;Bladee;Ecco2K"),
        ];
        let idx = ArtistIndex::build(tracks.iter().copied(), std::iter::empty(), std::iter::empty());
        assert_eq!(idx.artists_of_track(tracks[3].0, tracks[3].1), ["Varg²™", "Bladee", "Ecco2K"]);
    }

    #[test]
    fn learns_from_other_collaborations() {
        let idx = index(&["Frost Children", "Frost Children, The Deep", "Effie, Sebii & The Deep"]);
        assert_eq!(idx.artists_of("Frost Children, The Deep"), ["Frost Children", "The Deep"]);
        assert_eq!(idx.artists_of("Effie, Sebii & The Deep"), ["Effie", "Sebii", "The Deep"]);
    }

    #[test]
    fn filtering_finds_every_track_with_an_artist() {
        let tags = ["Skrillex", "Skrillex & Wuki", "Joker, Skrillex & Sleepnet", "skrillex feat. Someone", "Wuki", "Wukiz"];
        let idx = index(&tags);
        let by = |name: &str| -> Vec<&str> { tags.iter().copied().filter(|t| idx.track_has_artist(t, "", name)).collect() };
        assert_eq!(by("Skrillex"), ["Skrillex", "Skrillex & Wuki", "Joker, Skrillex & Sleepnet", "skrillex feat. Someone"]);
        assert_eq!(by("Wuki"), ["Skrillex & Wuki", "Wuki"]);
        // An old collaboration name still finds its own tracks.
        assert_eq!(by("Skrillex & Wuki"), ["Skrillex & Wuki"]);
    }

    #[test]
    fn credits_add_artists_and_teach_names() {
        // Shapes seen in a real library.
        let tracks = [
            ("Denzel Curry", "Denzel Curry\nMaxo Kream"),
            ("Denzel Curry & Kenny Beats", "Denzel Curry/Kenny Beats"),
            ("Babymorocco, Frost Children", "Babymorocco"),
            ("Ben Böhmer & Panama", "Ben Böhmer;Panama"),
            ("Christ Dillinger, Acid Souljah & Shadow Wizard Money Gang", "CHRIST DILLINGER; Shadow Wizard Money Gang; Acid Souljah; Dj Smokey"),
            ("Frost Children", ""),
            ("Thaiboy Digital", "Thaiboy Digital"),
            ("ECCO2K & Thaiboy Digital", "Thaiboy Digital/Ecco2k/Yung Lean"),
        ];
        let idx = ArtistIndex::build(tracks.iter().copied(), std::iter::empty(), std::iter::empty());
        let of = |i: usize| idx.artists_of_track(tracks[i].0, tracks[i].1);
        assert_eq!(of(0), ["Denzel Curry", "Maxo Kream"]);
        assert_eq!(of(1), ["Denzel Curry", "Kenny Beats"]);
        // Credits are incomplete here; the tag fills in the rest.
        assert_eq!(of(2), ["Babymorocco", "Frost Children"]);
        assert_eq!(of(3), ["Ben Böhmer", "Panama"]);
        assert_eq!(of(4), ["Christ Dillinger", "Acid Souljah", "Shadow Wizard Money Gang", "Dj Smokey"]);
        assert_eq!(of(7), ["ECCO2K", "Thaiboy Digital", "Yung Lean"]);
        // A credited name counts as known for tags without credits.
        assert_eq!(idx.artists_of("Panama & Someone New"), ["Panama", "Someone New"]);
    }

    #[test]
    fn exceptions_are_never_split() {
        // "Fire" and "Garfunkel" are artists here, but the built-in exceptions win.
        let idx = index(&["Fire", "Garfunkel", "Earth, Wind & Fire", "Simon & Garfunkel", "Tyler, the Creator, Kali Uchis"]);
        assert_eq!(idx.artists_of("Earth, Wind & Fire"), ["Earth, Wind & Fire"]);
        assert_eq!(idx.artists_of("Simon & Garfunkel"), ["Simon & Garfunkel"]);
        // An exception inside a collaboration is one of its artists.
        assert_eq!(idx.artists_of("Tyler, the Creator, Kali Uchis"), ["Tyler, the Creator", "Kali Uchis"]);
        assert_eq!(idx.artists_of("Earth, Wind & Fire feat. Garfunkel"), ["Earth, Wind & Fire", "Garfunkel"]);
        // The user's own exceptions.
        let tags = [("Bob", ""), ("Bob & Lisa", "")];
        let plain = ArtistIndex::build(tags.iter().copied(), std::iter::empty(), std::iter::empty());
        assert_eq!(plain.artists_of("Bob & Lisa"), ["Bob", "Lisa"]);
        let user = ArtistIndex::build(tags.iter().copied(), std::iter::empty(), ["bob & lisa"]);
        assert_eq!(user.artists_of("Bob & Lisa"), ["Bob & Lisa"]);
    }
}
