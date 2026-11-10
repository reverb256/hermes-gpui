//! Small JSON status channel between the running client and the Omarchy bar.
//!
//! The window owns the truth (connection, streaming, pending approvals); the
//! bar widget runs `hermes-desktop status --json`, which reads the snapshot the
//! window last wrote. No socket protocol changes, no daemon, and a stale file
//! simply reads as "not running".

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub host: String,
    pub profile: String,
    pub connected: bool,
    pub busy: bool,
    pub approvals: usize,
    pub session: Option<String>,
    pub last_reply: Option<String>,
}

pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("hermes-gpui")
}

pub fn status_path() -> PathBuf {
    cache_dir().join("status.json")
}

impl Snapshot {
    fn to_value(&self) -> Value {
        json!({
            "available": true,
            "host": self.host,
            "profile": self.profile,
            "connected": self.connected,
            "busy": self.busy,
            "approvals": self.approvals,
            "session": self.session,
            "lastReply": self.last_reply,
            "updated": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|age| age.as_secs())
                .unwrap_or(0),
        })
    }
}

/// Write the snapshot atomically so the bar never reads a half-written file.
pub fn publish(snapshot: &Snapshot) {
    publish_to(&cache_dir(), snapshot);
}

fn publish_to(dir: &Path, snapshot: &Snapshot) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let temporary = dir.join("status.json.tmp");
    if std::fs::write(&temporary, snapshot.to_value().to_string()).is_err() {
        return;
    }
    if std::fs::rename(&temporary, dir.join("status.json")).is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

/// The JSON the bar widget consumes; "not running" when nothing was written.
pub fn read_json() -> String {
    read_json_from(&status_path())
}

fn read_json_from(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) if serde_json::from_str::<Value>(&text).is_ok() => text,
        _ => unavailable(),
    }
}

fn unavailable() -> String {
    json!({
        "available": false,
        "connected": false,
        "busy": false,
        "approvals": 0
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|age| age.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "hermes-gpui-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample() -> Snapshot {
        Snapshot {
            host: "http://127.0.0.1:8790".into(),
            profile: "ops".into(),
            connected: true,
            busy: true,
            approvals: 2,
            session: Some("s_1".into()),
            last_reply: Some("done".into()),
        }
    }

    #[test]
    fn snapshots_serialize_the_fields_the_bar_reads() {
        let value = sample().to_value();
        assert_eq!(value["available"], json!(true));
        assert_eq!(value["connected"], json!(true));
        assert_eq!(value["approvals"], json!(2));
        assert_eq!(value["session"], json!("s_1"));
    }

    #[test]
    fn snapshot_round_trips_through_the_file_the_bar_reads() {
        let dir = scratch_dir("roundtrip");
        publish_to(&dir, &sample());
        let value: Value = serde_json::from_str(&read_json_from(&dir.join("status.json")))
            .expect("valid snapshot");
        assert_eq!(value["connected"], json!(true));
        assert_eq!(value["profile"], json!("ops"));
        assert_eq!(value["lastReply"], json!("done"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_corrupt_status_reads_as_unavailable() {
        let dir = scratch_dir("missing");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let value: Value =
            serde_json::from_str(&read_json_from(&dir.join("status.json"))).expect("valid json");
        assert_eq!(value["available"], json!(false));
        std::fs::write(dir.join("status.json"), "not json").expect("write corrupt file");
        let value: Value =
            serde_json::from_str(&read_json_from(&dir.join("status.json"))).expect("valid json");
        assert_eq!(value["available"], json!(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
