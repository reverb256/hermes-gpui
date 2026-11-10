use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch. Wall-clock time is what pairing, prompt
/// expiry and audit display need; monotonic durations stay in `Instant`.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

pub fn clamp_limit(limit: Option<u32>, default: u32, max: u32) -> u32 {
    limit.unwrap_or(default).clamp(1, max)
}
