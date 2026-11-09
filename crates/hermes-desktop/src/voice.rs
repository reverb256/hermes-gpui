//! Text-to-speech through the same `voice-models` service the stack already
//! runs (`POST /tts`, Chatterbox Turbo by default).
//!
//! The desktop deliberately does not bundle a speech model: the host service
//! owns the GPU model, voices and updates. `HERMES_TTS_URL` points at it — the
//! k3s NodePort or a `kubectl port-forward` — and `HERMES_TTS_VOICE` picks a
//! voice the service knows. Playback goes through the first available native
//! player (`pw-play`, `paplay`, `aplay`), so no audio stack is vendored.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub const DEFAULT_VOICE: &str = "en-US-AndrewNeural";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtsConfig {
    pub base_url: String,
    pub voice: String,
}

impl TtsConfig {
    pub fn from_env() -> Option<Self> {
        Self::parse(
            std::env::var("HERMES_TTS_URL").ok(),
            std::env::var("HERMES_TTS_VOICE").ok(),
        )
    }

    /// Trim and validate the env pair without touching process globals, so the
    /// parsing rules stay testable.
    pub fn parse(base_url: Option<String>, voice: Option<String>) -> Option<Self> {
        let base_url = base_url?.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return None;
        }
        let voice = voice
            .map(|voice| voice.trim().to_string())
            .filter(|voice| !voice.is_empty())
            .unwrap_or_else(|| DEFAULT_VOICE.to_string());
        Some(Self { base_url, voice })
    }
}

pub fn request_body(text: &str, voice: &str) -> Value {
    json!({ "text": text, "voice": voice })
}

/// Fetch a WAV from `POST /tts`; the response body is the audio itself.
pub async fn synthesize(config: &TtsConfig, text: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|err| format!("tts client: {err}"))?;
    let response = client
        .post(format!("{}/tts", config.base_url))
        .json(&request_body(text, &config.voice))
        .send()
        .await
        .map_err(|err| format!("tts request: {err}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|err| format!("tts response: {err}"))?;
    if !status.is_success() {
        let detail: String = String::from_utf8_lossy(&body).chars().take(300).collect();
        return Err(format!("tts {status}: {detail}"));
    }
    if body.is_empty() {
        return Err("tts returned an empty body".into());
    }
    Ok(body.to_vec())
}

/// First native player present on `PATH`, in preference order.
pub fn player() -> Option<&'static str> {
    player_from(&std::env::var("PATH").unwrap_or_default())
}

pub fn player_from(path: &str) -> Option<&'static str> {
    const CANDIDATES: [&str; 3] = ["pw-play", "paplay", "aplay"];
    CANDIDATES.into_iter().find(|candidate| {
        path.split(':')
            .filter(|dir| !dir.is_empty())
            .any(|dir| Path::new(dir).join(candidate).is_file())
    })
}

/// A unique temp file next to the system temp dir; the backend removes it
/// after playback.
pub fn temp_wav_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("hermes-tts-{}-{nanos}.wav", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_needs_a_base_url_and_trims_both_values() {
        assert!(TtsConfig::parse(None, None).is_none());
        assert!(TtsConfig::parse(Some("  ".into()), None).is_none());
        assert_eq!(
            TtsConfig::parse(Some(" http://voice.local:8004/ ".into()), Some("  ".into())),
            Some(TtsConfig {
                base_url: "http://voice.local:8004".into(),
                voice: DEFAULT_VOICE.into(),
            })
        );
        assert_eq!(
            TtsConfig::parse(
                Some("http://voice.local:8004".into()),
                Some(" en-GB-SoniaNeural ".into())
            )
            .unwrap()
            .voice,
            "en-GB-SoniaNeural"
        );
    }

    #[test]
    fn request_body_matches_the_voice_models_contract() {
        assert_eq!(
            request_body("hello", "en-US-AndrewNeural"),
            json!({ "text": "hello", "voice": "en-US-AndrewNeural" })
        );
    }

    #[test]
    fn players_are_found_in_preference_order() {
        let dir = std::env::temp_dir().join(format!("hermes-players-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create player dir");
        std::fs::write(dir.join("paplay"), []).expect("paplay fixture");
        std::fs::write(dir.join("aplay"), []).expect("aplay fixture");
        let path = dir.to_string_lossy().to_string();
        assert_eq!(player_from(&path), Some("paplay"));
        assert_eq!(player_from(""), None);
        assert_eq!(player_from("/definitely/missing"), None);

        std::fs::write(dir.join("pw-play"), []).expect("pw-play fixture");
        assert_eq!(player_from(&path), Some("pw-play"));
        std::fs::remove_dir_all(&dir).expect("clean player dir");
    }

    #[test]
    fn temp_paths_are_unique_wavs() {
        let first = temp_wav_path();
        let second = temp_wav_path();
        assert_eq!(first.extension().and_then(|ext| ext.to_str()), Some("wav"));
        assert_ne!(first, second);
    }
}
