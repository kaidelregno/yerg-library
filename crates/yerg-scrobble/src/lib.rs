//! Scrobbling to Last.fm, ListenBrainz and Libre.fm.
//!
//! Every listen is first written to the library's `plays` table; the
//! [`worker`] drains pending rows whenever the network allows, so listens made
//! offline (or imported from the MP3 player) are submitted later with their
//! original timestamps.

pub mod lastfm;
pub mod listenbrainz;
pub mod worker;

use yerg_core::model::PlayWithTrack;

/// What a service needs to know about one listen.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrobbleItem {
    pub play_id: i64,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub album_artist: String,
    pub track_no: Option<i32>,
    pub duration_secs: Option<u32>,
    /// Unix seconds when the listen started.
    pub timestamp: i64,
}

impl From<&PlayWithTrack> for ScrobbleItem {
    fn from(p: &PlayWithTrack) -> Self {
        ScrobbleItem {
            play_id: p.play.id,
            artist: p.artist.clone(),
            title: p.title.clone(),
            album: p.album.clone(),
            album_artist: p.album_artist.clone(),
            track_no: p.track_no,
            duration_secs: if p.duration_ms > 0 {
                Some((p.duration_ms / 1000) as u32)
            } else {
                None
            },
            timestamp: p.play.started_at,
        }
    }
}

impl ScrobbleItem {
    /// Services refuse listens without an artist and title.
    pub fn is_submittable(&self) -> bool {
        !self.artist.trim().is_empty() && !self.title.trim().is_empty()
    }
}

/// Per-item result of a submission.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Accepted,
    /// The service took the request but will not count this listen.
    Ignored(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ScrobbleError {
    #[error("network error: {0}")]
    Network(String),
    #[error("not authenticated: {0}")]
    Auth(String),
    #[error("service error {code}: {message}")]
    Service { code: i64, message: String },
    #[error("rate limited, retry later")]
    RateLimited,
    /// Last.fm / Libre.fm desktop auth: the request token has not been approved in the browser yet.
    #[error("waiting for approval in the browser")]
    AwaitingApproval,
    #[error("{0}")]
    Other(String),
}

impl ScrobbleError {
    /// Whether the pending listens should stay queued and be retried.
    pub fn is_retryable(&self) -> bool {
        match self {
            ScrobbleError::Network(_) | ScrobbleError::RateLimited | ScrobbleError::AwaitingApproval => true,
            ScrobbleError::Service { code, .. } => matches!(code, 8 | 11 | 16 | 29),
            ScrobbleError::Auth(_) => false,
            ScrobbleError::Other(_) => true,
        }
    }
}

pub type ScrobbleResult<T> = Result<T, ScrobbleError>;

/// A scrobbling backend.
pub trait Service: Send + Sync {
    fn name(&self) -> &'static str;
    /// Submit a batch; the returned vector lines up with `items`.
    fn scrobble(&self, items: &[ScrobbleItem]) -> ScrobbleResult<Vec<Outcome>>;
    fn now_playing(&self, item: &ScrobbleItem) -> ScrobbleResult<()>;
    /// Largest batch the service accepts per request.
    fn batch_size(&self) -> usize;
}

pub(crate) fn map_ureq_error(e: ureq::Error) -> ScrobbleError {
    match e {
        ureq::Error::Status(429, _) => ScrobbleError::RateLimited,
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            if code == 401 || code == 403 {
                ScrobbleError::Auth(format!("HTTP {code}: {body}"))
            } else if (500..600).contains(&code) {
                ScrobbleError::Network(format!("HTTP {code}: {body}"))
            } else {
                ScrobbleError::Other(format!("HTTP {code}: {body}"))
            }
        }
        ureq::Error::Transport(t) => ScrobbleError::Network(t.to_string()),
    }
}

pub(crate) fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("YergLibrary/", env!("CARGO_PKG_VERSION")))
        .build()
}
