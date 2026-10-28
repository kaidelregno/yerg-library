//! ListenBrainz submission API (https://listenbrainz.readthedocs.io/).
//! Only a user token is needed; historical listens are accepted at any age.

use serde_json::{json, Value};

use crate::{agent, map_ureq_error, Outcome, ScrobbleError, ScrobbleItem, ScrobbleResult, Service};

pub const DEFAULT_API_ROOT: &str = "https://api.listenbrainz.org";

#[derive(Debug, Clone)]
pub struct ListenBrainz {
    pub token: String,
    pub api_root: String,
}

impl ListenBrainz {
    pub fn new(token: impl Into<String>) -> ListenBrainz {
        ListenBrainz {
            token: token.into(),
            api_root: DEFAULT_API_ROOT.to_string(),
        }
    }

    /// Check the token; returns the user name it belongs to.
    pub fn validate_token(&self) -> ScrobbleResult<String> {
        let resp = agent()
            .get(&format!("{}/1/validate-token", self.api_root))
            .set("Authorization", &format!("Token {}", self.token))
            .call()
            .map_err(map_ureq_error)?;
        let v: Value = resp.into_json().map_err(|e| ScrobbleError::Other(e.to_string()))?;
        if v["valid"].as_bool() == Some(true) {
            Ok(v["user_name"].as_str().unwrap_or("").to_string())
        } else {
            Err(ScrobbleError::Auth(
                v["message"].as_str().unwrap_or("invalid token").to_string(),
            ))
        }
    }

    fn submit(&self, listen_type: &str, payload: Vec<Value>) -> ScrobbleResult<()> {
        let body = json!({ "listen_type": listen_type, "payload": payload });
        agent()
            .post(&format!("{}/1/submit-listens", self.api_root))
            .set("Authorization", &format!("Token {}", self.token))
            .send_json(body)
            .map_err(map_ureq_error)?;
        Ok(())
    }
}

fn track_metadata(item: &ScrobbleItem) -> Value {
    let mut additional = json!({
        "media_player": "Yerg Library",
        "submission_client": "Yerg Library",
        "submission_client_version": env!("CARGO_PKG_VERSION"),
    });
    if let Some(d) = item.duration_secs.filter(|d| *d > 0) {
        additional["duration_ms"] = json!(d as u64 * 1000);
    }
    if let Some(n) = item.track_no.filter(|n| *n > 0) {
        additional["tracknumber"] = json!(n);
    }
    let mut meta = json!({
        "artist_name": item.artist,
        "track_name": item.title,
        "additional_info": additional,
    });
    if !item.album.is_empty() {
        meta["release_name"] = json!(item.album);
    }
    meta
}

impl Service for ListenBrainz {
    fn name(&self) -> &'static str {
        "ListenBrainz"
    }

    fn batch_size(&self) -> usize {
        200
    }

    fn scrobble(&self, items: &[ScrobbleItem]) -> ScrobbleResult<Vec<Outcome>> {
        if items.is_empty() {
            return Ok(vec![]);
        }
        let payload: Vec<Value> = items
            .iter()
            .map(|it| {
                json!({
                    "listened_at": it.timestamp,
                    "track_metadata": track_metadata(it),
                })
            })
            .collect();
        let listen_type = if items.len() == 1 { "single" } else { "import" };
        self.submit(listen_type, payload)?;
        Ok(vec![Outcome::Accepted; items.len()])
    }

    fn now_playing(&self, item: &ScrobbleItem) -> ScrobbleResult<()> {
        self.submit(
            "playing_now",
            vec![json!({ "track_metadata": track_metadata(item) })],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_shape() {
        let item = ScrobbleItem {
            play_id: 1,
            artist: "Tycho".into(),
            title: "Awake".into(),
            album: "Awake".into(),
            album_artist: "Tycho".into(),
            track_no: Some(1),
            duration_secs: Some(275),
            timestamp: 1_700_000_000,
        };
        let m = track_metadata(&item);
        assert_eq!(m["artist_name"], "Tycho");
        assert_eq!(m["release_name"], "Awake");
        assert_eq!(m["additional_info"]["duration_ms"], 275_000);
    }
}
