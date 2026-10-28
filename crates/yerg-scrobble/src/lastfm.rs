//! The Last.fm scrobbling API 2.0, spoken by Last.fm and by Libre.fm.
//!
//! Last.fm: users supply their own API key and secret
//! (https://www.last.fm/api/account/create), then authorise the app in a
//! browser once; the session key is kept in settings.
//!
//! Libre.fm runs the same API on its own server (libre.fm/2.0/, approval page
//! libre.fm/api/auth/) and does not issue API keys: any key and secret are
//! accepted, so the app uses its own placeholders and the user only approves
//! the app in the browser.

use serde_json::Value;

use crate::{agent, map_ureq_error, Outcome, ScrobbleError, ScrobbleItem, ScrobbleResult, Service};

/// A server that speaks the Last.fm API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    LastFm,
    LibreFm,
}

impl Network {
    pub fn api_root(self) -> &'static str {
        match self {
            Network::LastFm => "https://ws.audioscrobbler.com/2.0/",
            Network::LibreFm => "https://libre.fm/2.0/",
        }
    }

    fn auth_page(self) -> &'static str {
        match self {
            Network::LastFm => "https://www.last.fm/api/auth/",
            Network::LibreFm => "https://libre.fm/api/auth/",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Network::LastFm => "Last.fm",
            Network::LibreFm => "Libre.fm",
        }
    }
}

/// Libre.fm accepts any API key and secret; these identify the app.
pub const LIBREFM_API_KEY: &str = "yerglibrary000000000000000000000";
pub const LIBREFM_API_SECRET: &str = "yerglibrary000000000000000000000";

#[derive(Debug, Clone)]
pub struct LastFm {
    pub network: Network,
    pub api_key: String,
    pub api_secret: String,
    pub session_key: String,
}

impl LastFm {
    /// A Libre.fm connection for an approved session.
    pub fn librefm(session_key: String) -> LastFm {
        LastFm {
            network: Network::LibreFm,
            api_key: LIBREFM_API_KEY.into(),
            api_secret: LIBREFM_API_SECRET.into(),
            session_key,
        }
    }
}

/// Step 1 of desktop auth: a request token.
pub fn get_token(network: Network, api_key: &str, api_secret: &str) -> ScrobbleResult<String> {
    let v = call(network, api_key, api_secret, "auth.getToken", &[])?;
    v["token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| ScrobbleError::Other("no token in response".into()))
}

/// Step 2: the page the user must approve.
pub fn auth_url(network: Network, api_key: &str, token: &str) -> String {
    format!("{}?api_key={api_key}&token={token}", network.auth_page())
}

/// Step 3: exchange the approved token for a session key.  Returns (session key, username).
pub fn get_session(network: Network, api_key: &str, api_secret: &str, token: &str) -> ScrobbleResult<(String, String)> {
    let v = call(network, api_key, api_secret, "auth.getSession", &[("token", token)])?;
    let key = v["session"]["key"].as_str().unwrap_or_default().to_string();
    let name = v["session"]["name"].as_str().unwrap_or_default().to_string();
    if key.is_empty() {
        return Err(ScrobbleError::Auth("no session key in response".into()));
    }
    Ok((key, name))
}

/// api_sig = md5(sorted key+value pairs concatenated, then the secret)
pub fn sign(params: &[(String, String)], secret: &str) -> String {
    let mut sorted: Vec<&(String, String)> = params.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut s = String::new();
    for (k, v) in sorted {
        s.push_str(k);
        s.push_str(v);
    }
    s.push_str(secret);
    format!("{:x}", md5::compute(s.as_bytes()))
}

fn call(network: Network, api_key: &str, api_secret: &str, method: &str, extra: &[(&str, &str)]) -> ScrobbleResult<Value> {
    let mut params: Vec<(String, String)> = vec![
        ("method".into(), method.into()),
        ("api_key".into(), api_key.into()),
    ];
    for (k, v) in extra {
        params.push((k.to_string(), v.to_string()));
    }
    let sig = sign(&params, api_secret);
    params.push(("api_sig".into(), sig));
    params.push(("format".into(), "json".into()));
    post(network, &params)
}

fn post(network: Network, params: &[(String, String)]) -> ScrobbleResult<Value> {
    let form: Vec<(&str, &str)> = params.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let resp = agent().post(network.api_root()).send_form(&form);
    let body: Value = match resp {
        Ok(r) => r.into_json().map_err(|e| ScrobbleError::Other(e.to_string()))?,
        Err(ureq::Error::Status(code, r)) => {
            // Last.fm returns JSON error bodies with 4xx codes.
            match r.into_json::<Value>() {
                Ok(v) if v.get("error").is_some() => v,
                Ok(v) => return Err(ScrobbleError::Other(format!("HTTP {code}: {v}"))),
                Err(e) => return Err(ScrobbleError::Network(format!("HTTP {code}: {e}"))),
            }
        }
        Err(e) => return Err(map_ureq_error(e)),
    };
    if let Some(code) = body.get("error").and_then(|e| e.as_i64()) {
        let message = body["message"].as_str().unwrap_or("unknown error").to_string();
        return Err(error_for_code(code, message));
    }
    Ok(body)
}

impl LastFm {
    fn signed_call(&self, method: &str, extra: Vec<(String, String)>) -> ScrobbleResult<Value> {
        let mut params: Vec<(String, String)> = vec![
            ("method".into(), method.into()),
            ("api_key".into(), self.api_key.clone()),
            ("sk".into(), self.session_key.clone()),
        ];
        params.extend(extra);
        let sig = sign(&params, &self.api_secret);
        params.push(("api_sig".into(), sig));
        params.push(("format".into(), "json".into()));
        post(self.network, &params)
    }
}

impl Service for LastFm {
    fn name(&self) -> &'static str {
        self.network.name()
    }

    fn batch_size(&self) -> usize {
        50
    }

    fn scrobble(&self, items: &[ScrobbleItem]) -> ScrobbleResult<Vec<Outcome>> {
        if items.is_empty() {
            return Ok(vec![]);
        }
        let mut params = Vec::new();
        for (i, it) in items.iter().enumerate() {
            params.push((format!("artist[{i}]"), it.artist.clone()));
            params.push((format!("track[{i}]"), it.title.clone()));
            params.push((format!("timestamp[{i}]"), it.timestamp.to_string()));
            if !it.album.is_empty() {
                params.push((format!("album[{i}]"), it.album.clone()));
            }
            if !it.album_artist.is_empty() && it.album_artist != it.artist {
                params.push((format!("albumArtist[{i}]"), it.album_artist.clone()));
            }
            if let Some(n) = it.track_no.filter(|n| *n > 0) {
                params.push((format!("trackNumber[{i}]"), n.to_string()));
            }
            if let Some(d) = it.duration_secs.filter(|d| *d > 0) {
                params.push((format!("duration[{i}]"), d.to_string()));
            }
            params.push((format!("chosenByUser[{i}]"), "1".into()));
        }
        let v = self.signed_call("track.scrobble", params)?;
        parse_scrobble_response(&v, items.len())
    }

    fn now_playing(&self, item: &ScrobbleItem) -> ScrobbleResult<()> {
        let mut params = vec![
            ("artist".to_string(), item.artist.clone()),
            ("track".to_string(), item.title.clone()),
        ];
        if !item.album.is_empty() {
            params.push(("album".into(), item.album.clone()));
        }
        if let Some(d) = item.duration_secs.filter(|d| *d > 0) {
            params.push(("duration".into(), d.to_string()));
        }
        self.signed_call("track.updateNowPlaying", params).map(|_| ())
    }
}

/// Map a Last.fm API error code (https://www.last.fm/api/errorcodes) to our error kinds.
fn error_for_code(code: i64, message: String) -> ScrobbleError {
    match code {
        // auth.getSession before the user approved the token in the browser
        14 => ScrobbleError::AwaitingApproval,
        // invalid service, auth failed, bad key, suspended key, bad signature, expired token
        4 | 9 | 10 | 13 | 15 | 26 => ScrobbleError::Auth(format!("{code}: {message}")),
        29 => ScrobbleError::RateLimited,
        _ => ScrobbleError::Service { code, message },
    }
}

/// The `scrobbles.scrobble` element is an object for one item and an array for several.
fn parse_scrobble_response(v: &Value, n: usize) -> ScrobbleResult<Vec<Outcome>> {
    let node = &v["scrobbles"]["scrobble"];
    let entries: Vec<&Value> = match node {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![node],
        _ => return Err(ScrobbleError::Other(format!("unexpected response: {v}"))),
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = entries.get(i).copied().unwrap_or(&Value::Null);
        let code = e["ignoredMessage"]["code"]
            .as_str()
            .and_then(|c| c.parse::<i64>().ok())
            .or_else(|| e["ignoredMessage"]["code"].as_i64())
            .unwrap_or(0);
        if code == 0 {
            out.push(Outcome::Accepted);
        } else {
            let text = e["ignoredMessage"]["#text"].as_str().unwrap_or("");
            let reason = match code {
                1 => "artist was ignored".to_string(),
                2 => "track was ignored".to_string(),
                3 => "timestamp too old".to_string(),
                4 => "timestamp too new".to_string(),
                5 => "daily scrobble limit exceeded".to_string(),
                _ => format!("ignored ({code}) {text}"),
            };
            out.push(Outcome::Ignored(reason));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_reference() {
        // From the Last.fm docs: md5 of "api_keyxxxmethodauth.getSessiontokenyyySECRET"
        let params = vec![
            ("method".to_string(), "auth.getSession".to_string()),
            ("token".to_string(), "yyy".to_string()),
            ("api_key".to_string(), "xxx".to_string()),
        ];
        let expected = format!("{:x}", md5::compute(b"api_keyxxxmethodauth.getSessiontokenyyySECRET"));
        assert_eq!(sign(&params, "SECRET"), expected);
    }

    #[test]
    fn networks() {
        assert_eq!(Network::LastFm.api_root(), "https://ws.audioscrobbler.com/2.0/");
        assert_eq!(Network::LibreFm.api_root(), "https://libre.fm/2.0/");
        assert_eq!(auth_url(Network::LastFm, "k", "t"), "https://www.last.fm/api/auth/?api_key=k&token=t");
        assert_eq!(auth_url(Network::LibreFm, "k", "t"), "https://libre.fm/api/auth/?api_key=k&token=t");
        assert_eq!(LIBREFM_API_KEY.len(), 32, "same shape as a Last.fm key");
        assert_eq!(LastFm::librefm("s".into()).name(), "Libre.fm");
    }

    #[test]
    fn error_codes() {
        assert!(matches!(error_for_code(14, "Unauthorized Token".into()), ScrobbleError::AwaitingApproval));
        assert!(error_for_code(14, String::new()).is_retryable());
        assert!(matches!(error_for_code(9, "Invalid session key".into()), ScrobbleError::Auth(_)));
        assert!(!error_for_code(15, "expired".into()).is_retryable());
        assert!(matches!(error_for_code(29, String::new()), ScrobbleError::RateLimited));
        assert!(error_for_code(11, "offline".into()).is_retryable());
    }

    #[test]
    fn parses_mixed_outcomes() {
        let v: Value = serde_json::from_str(
            r##"{"scrobbles":{"@attr":{"accepted":1,"ignored":1},"scrobble":[
                {"ignoredMessage":{"code":"0","#text":""}},
                {"ignoredMessage":{"code":"3","#text":"Timestamp too old"}}]}}"##,
        )
        .unwrap();
        let out = parse_scrobble_response(&v, 2).unwrap();
        assert_eq!(out[0], Outcome::Accepted);
        assert!(matches!(out[1], Outcome::Ignored(_)));

        let single: Value = serde_json::from_str(
            r##"{"scrobbles":{"scrobble":{"ignoredMessage":{"code":"0","#text":""}}}}"##,
        )
        .unwrap();
        assert_eq!(parse_scrobble_response(&single, 1).unwrap(), vec![Outcome::Accepted]);
    }
}
