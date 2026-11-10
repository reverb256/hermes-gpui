//! SQLite persistence for the single-owner hub.
//!
//! One connection behind a mutex keeps the schema simple and serialises writes;
//! the workload is a personal node registry, so that is the right trade. Command
//! events are retained by sequence so a device can replay after reconnecting.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use hermes_protocol::{
    AuditEntry, Capabilities, CommandEvent, CommandRecord, CommandRequest, CommandStatus, HostInfo,
    NodeSummary, SecretPrompt, StreamChannel,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::util::now_ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    Node,
    Device,
}

impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Device => "device",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "node" => Some(Self::Node),
            "device" => Some(Self::Device),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Principal {
    pub id: String,
    pub kind: PrincipalKind,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Enrollment {
    pub kind: PrincipalKind,
    pub name: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct StoredPrompt {
    pub prompt: SecretPrompt,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub event: CommandEvent,
    pub at_ms: u64,
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create database directory {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open SQLite database {}", path.display()))?;
        Self::initialize(conn)
    }

    pub fn open_memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;

            CREATE TABLE IF NOT EXISTS enrollments (
                code_hash   TEXT PRIMARY KEY,
                kind        TEXT NOT NULL,
                name        TEXT NOT NULL,
                expires_at_ms INTEGER NOT NULL,
                used_at_ms  INTEGER
            );

            CREATE TABLE IF NOT EXISTS principals (
                id           TEXT PRIMARY KEY,
                kind         TEXT NOT NULL,
                name         TEXT NOT NULL,
                token_hash   TEXT NOT NULL UNIQUE,
                created_at_ms INTEGER NOT NULL,
                revoked_at_ms INTEGER
            );

            CREATE TABLE IF NOT EXISTS nodes (
                id             TEXT PRIMARY KEY,
                name           TEXT NOT NULL,
                hostname       TEXT NOT NULL,
                os             TEXT NOT NULL,
                arch           TEXT NOT NULL,
                agent_version  TEXT NOT NULL,
                capabilities   TEXT NOT NULL,
                labels         TEXT NOT NULL,
                last_seen_ms   INTEGER NOT NULL,
                online         INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS commands (
                id             TEXT PRIMARY KEY,
                node_id        TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                argv           TEXT NOT NULL,
                cwd            TEXT,
                status         TEXT NOT NULL,
                created_at_ms  INTEGER NOT NULL,
                started_at_ms  INTEGER,
                finished_at_ms INTEGER,
                exit_code      INTEGER,
                message        TEXT,
                UNIQUE(node_id, idempotency_key)
            );

            CREATE TABLE IF NOT EXISTS command_events (
                command_id TEXT NOT NULL,
                seq        INTEGER NOT NULL,
                channel    TEXT NOT NULL,
                data_b64   TEXT NOT NULL,
                at_ms      INTEGER NOT NULL,
                PRIMARY KEY(command_id, seq)
            );

            CREATE TABLE IF NOT EXISTS prompts (
                id            TEXT PRIMARY KEY,
                node_id       TEXT NOT NULL,
                command_id    TEXT NOT NULL,
                challenge     TEXT NOT NULL,
                requester     TEXT NOT NULL,
                purpose       TEXT NOT NULL,
                scope         TEXT NOT NULL,
                env_var       TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                expires_at_ms INTEGER NOT NULL,
                resolved_at_ms INTEGER,
                outcome       TEXT
            );

            CREATE TABLE IF NOT EXISTS audit (
                id          TEXT PRIMARY KEY,
                at_ms       INTEGER NOT NULL,
                actor       TEXT NOT NULL,
                kind        TEXT NOT NULL,
                detail      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS audit_at ON audit(at_ms DESC);
            CREATE INDEX IF NOT EXISTS commands_node ON commands(node_id, created_at_ms DESC);

            -- A prompt challenge only exists in the previous process's memory.
            -- Fail closed on restart instead of showing a stale card.
            UPDATE prompts
               SET resolved_at_ms = CAST(strftime('%s','now') AS INTEGER) * 1000,
                   outcome = 'hub_restarted'
             WHERE resolved_at_ms IS NULL;
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn healthy(&self) -> bool {
        self.conn()
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .is_ok()
    }

    pub fn create_enrollment(
        &self,
        code_hash: &str,
        kind: PrincipalKind,
        name: &str,
        expires_at_ms: u64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO enrollments(code_hash, kind, name, expires_at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![code_hash, kind.as_str(), name, expires_at_ms as i64],
        )?;
        Ok(())
    }

    /// Consume a code atomically; an expired or already-used code returns None.
    pub fn consume_enrollment(&self, code_hash: &str, now: u64) -> Result<Option<Enrollment>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let row: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT kind, name, expires_at_ms FROM enrollments
                 WHERE code_hash = ?1 AND used_at_ms IS NULL AND expires_at_ms > ?2",
                params![code_hash, now as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((kind, name, expires_at_ms)) = row else {
            return Ok(None);
        };
        let kind = PrincipalKind::parse(&kind).context("unknown enrollment kind")?;
        tx.execute(
            "UPDATE enrollments SET used_at_ms = ?1 WHERE code_hash = ?2 AND used_at_ms IS NULL",
            params![now as i64, code_hash],
        )?;
        tx.commit()?;
        Ok(Some(Enrollment {
            kind,
            name,
            expires_at_ms: expires_at_ms as u64,
        }))
    }

    pub fn create_principal(
        &self,
        id: &str,
        kind: PrincipalKind,
        name: &str,
        token_hash: &str,
        now: u64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO principals(id, kind, name, token_hash, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, kind.as_str(), name, token_hash, now as i64],
        )?;
        Ok(())
    }

    pub fn principal_by_token_hash(&self, token_hash: &str) -> Result<Option<Principal>> {
        let row = self
            .conn()
            .query_row(
                "SELECT id, kind, name FROM principals
                 WHERE token_hash = ?1 AND revoked_at_ms IS NULL",
                params![token_hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(id, kind, name)| {
            let kind = PrincipalKind::parse(&kind).context("unknown principal kind")?;
            Ok(Principal { id, kind, name })
        })
        .transpose()
    }

    pub fn principal(&self, id: &str) -> Result<Option<Principal>> {
        let row = self
            .conn()
            .query_row(
                "SELECT id, kind, name FROM principals WHERE id = ?1 AND revoked_at_ms IS NULL",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(id, kind, name)| {
            let kind = PrincipalKind::parse(&kind).context("unknown principal kind")?;
            Ok(Principal { id, kind, name })
        })
        .transpose()
    }

    pub fn principals(&self, kind: PrincipalKind) -> Result<Vec<Principal>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, kind, name FROM principals
             WHERE kind = ?1 AND revoked_at_ms IS NULL ORDER BY name COLLATE NOCASE",
        )?;
        let rows = statement.query_map(params![kind.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut principals = Vec::new();
        for row in rows {
            let (id, kind, name) = row?;
            principals.push(Principal {
                id,
                kind: PrincipalKind::parse(&kind).context("unknown principal kind")?,
                name,
            });
        }
        Ok(principals)
    }

    pub fn revoke_principal(&self, id: &str, now: u64) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE principals SET revoked_at_ms = ?1 WHERE id = ?2 AND revoked_at_ms IS NULL",
            params![now as i64, id],
        )?;
        Ok(changed > 0)
    }

    pub fn upsert_node(
        &self,
        id: &str,
        name: &str,
        host: &HostInfo,
        capabilities: &Capabilities,
        labels: &BTreeMap<String, String>,
        now: u64,
    ) -> Result<()> {
        let caps = serde_json::to_string(capabilities)?;
        let labels = serde_json::to_string(labels)?;
        self.conn().execute(
            "INSERT INTO nodes(id, name, hostname, os, arch, agent_version, capabilities, labels, last_seen_ms, online)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                hostname = excluded.hostname,
                os = excluded.os,
                arch = excluded.arch,
                agent_version = excluded.agent_version,
                capabilities = excluded.capabilities,
                labels = excluded.labels,
                last_seen_ms = excluded.last_seen_ms,
                online = 1",
            params![
                id,
                name,
                host.hostname,
                host.os,
                host.arch,
                host.agent_version,
                caps,
                labels,
                now as i64
            ],
        )?;
        Ok(())
    }

    pub fn set_node_online(&self, id: &str, online: bool, now: u64) -> Result<()> {
        self.conn().execute(
            "UPDATE nodes SET online = ?1, last_seen_ms = ?2 WHERE id = ?3",
            params![i64::from(online), now as i64, id],
        )?;
        Ok(())
    }

    pub fn touch_node(&self, id: &str, now: u64) -> Result<()> {
        self.conn().execute(
            "UPDATE nodes SET last_seen_ms = ?1, online = 1 WHERE id = ?2",
            params![now as i64, id],
        )?;
        Ok(())
    }

    pub fn node(&self, id: &str) -> Result<Option<NodeSummary>> {
        let row = self
            .conn()
            .query_row(
                "SELECT id, name, hostname, os, arch, agent_version, capabilities, labels,
                        online, last_seen_ms
                 FROM nodes WHERE id = ?1",
                params![id],
                node_from_row,
            )
            .optional()?;
        row.transpose()
    }

    pub fn nodes(&self) -> Result<Vec<NodeSummary>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, name, hostname, os, arch, agent_version, capabilities, labels,
                    online, last_seen_ms
             FROM nodes ORDER BY name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], node_from_row)?;
        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row??);
        }
        Ok(nodes)
    }

    /// Insert a command, or return the first record for this idempotency key.
    pub fn create_command(
        &self,
        node_id: &str,
        id: &str,
        request: &CommandRequest,
        now: u64,
    ) -> Result<(CommandRecord, bool)> {
        let argv = serde_json::to_string(&request.argv)?;
        let inserted = self.conn().execute(
            "INSERT OR IGNORE INTO commands(
                id, node_id, idempotency_key, argv, cwd, status, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6)",
            params![
                id,
                node_id,
                request.idempotency_key,
                argv,
                request.cwd,
                now as i64
            ],
        )?;
        let record = self
            .command_by_key(node_id, &request.idempotency_key)?
            .context("command disappeared after insert")?;
        Ok((record, inserted > 0))
    }

    fn command_by_key(&self, node_id: &str, key: &str) -> Result<Option<CommandRecord>> {
        let record = self
            .conn()
            .query_row(
                "SELECT id, node_id, argv, cwd, status, idempotency_key, created_at_ms,
                        started_at_ms, finished_at_ms, exit_code, message
                 FROM commands WHERE node_id = ?1 AND idempotency_key = ?2",
                params![node_id, key],
                command_from_row,
            )
            .optional()?;
        record.transpose()
    }

    pub fn command(&self, id: &str) -> Result<Option<CommandRecord>> {
        let record = self
            .conn()
            .query_row(
                "SELECT id, node_id, argv, cwd, status, idempotency_key, created_at_ms,
                        started_at_ms, finished_at_ms, exit_code, message
                 FROM commands WHERE id = ?1",
                params![id],
                command_from_row,
            )
            .optional()?;
        record.transpose()
    }

    pub fn command_node(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT node_id FROM commands WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn mark_command_running(&self, id: &str, now: u64) -> Result<()> {
        self.conn().execute(
            "UPDATE commands SET status = 'running', started_at_ms = COALESCE(started_at_ms, ?1)
             WHERE id = ?2 AND status = 'queued'",
            params![now as i64, id],
        )?;
        Ok(())
    }

    /// First terminal result wins; a late duplicate cannot rewrite history.
    pub fn finish_command(
        &self,
        id: &str,
        status: CommandStatus,
        exit_code: Option<i32>,
        message: Option<&str>,
        now: u64,
    ) -> Result<Option<CommandRecord>> {
        let conn = self.conn();
        conn.execute(
            "UPDATE commands SET status = ?1, exit_code = ?2, message = ?3, finished_at_ms = ?4
             WHERE id = ?5 AND status NOT IN ('succeeded','failed','denied','timed_out','cancelled')",
            params![
                status.as_str(),
                exit_code,
                message,
                now as i64,
                id
            ],
        )?;
        drop(conn);
        self.command(id)
    }

    /// Fail unfinished commands after their node disconnects.
    pub fn fail_commands_for_node(
        &self,
        node_id: &str,
        message: &str,
        now: u64,
    ) -> Result<Vec<CommandRecord>> {
        let ids: Vec<String> = {
            let conn = self.conn();
            let mut statement = conn.prepare(
                "SELECT id FROM commands
                 WHERE node_id = ?1 AND status IN ('queued','running')",
            )?;
            let rows = statement.query_map(params![node_id], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut failed = Vec::new();
        for id in ids {
            self.conn().execute(
                "UPDATE commands SET status = 'failed', message = ?1, finished_at_ms = ?2
                 WHERE id = ?3 AND status IN ('queued','running')",
                params![message, now as i64, id],
            )?;
            if let Some(record) = self.command(&id)? {
                failed.push(record);
            }
        }
        Ok(failed)
    }

    pub fn insert_command_event(
        &self,
        command_id: &str,
        seq: u64,
        channel: StreamChannel,
        data_b64: &str,
        now: u64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO command_events(command_id, seq, channel, data_b64, at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                command_id,
                seq as i64,
                channel.as_str(),
                data_b64,
                now as i64
            ],
        )?;
        Ok(())
    }

    pub fn command_events(
        &self,
        command_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<StoredEvent>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT seq, channel, data_b64, at_ms FROM command_events
             WHERE command_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = statement.query_map(params![command_id, after as i64, limit], |row| {
            let channel: String = row.get(1)?;
            let at_ms: i64 = row.get(3)?;
            Ok((
                row.get::<_, i64>(0)?,
                channel,
                row.get::<_, String>(2)?,
                at_ms,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (seq, channel, data_b64, at_ms) = row?;
            let channel = StreamChannel::parse(&channel).context("unknown stream channel")?;
            events.push(StoredEvent {
                event: CommandEvent {
                    seq: seq as u64,
                    channel,
                    data_b64,
                    at_ms: at_ms as u64,
                },
                at_ms: at_ms as u64,
            });
        }
        Ok(events)
    }

    pub fn insert_prompt(&self, prompt: &SecretPrompt) -> Result<()> {
        self.conn().execute(
            "INSERT INTO prompts(id, node_id, command_id, challenge, requester, purpose, scope,
                                 env_var, created_at_ms, expires_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                prompt.id,
                prompt.node_id,
                prompt.command_id,
                prompt.challenge,
                prompt.requester,
                prompt.purpose,
                prompt.scope,
                prompt.env_var,
                prompt.created_at_ms as i64,
                prompt.expires_at_ms as i64
            ],
        )?;
        Ok(())
    }

    pub fn prompt(&self, id: &str) -> Result<Option<StoredPrompt>> {
        let row = self
            .conn()
            .query_row(
                "SELECT id, node_id, command_id, challenge, requester, purpose, scope, env_var,
                        created_at_ms, expires_at_ms, outcome
                 FROM prompts WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        SecretPrompt {
                            id: row.get(0)?,
                            node_id: row.get(1)?,
                            command_id: row.get(2)?,
                            challenge: row.get(3)?,
                            requester: row.get(4)?,
                            purpose: row.get(5)?,
                            scope: row.get(6)?,
                            env_var: row.get(7)?,
                            created_at_ms: row.get::<_, i64>(8)? as u64,
                            expires_at_ms: row.get::<_, i64>(9)? as u64,
                        },
                        row.get::<_, Option<String>>(10)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.map(|(prompt, outcome)| StoredPrompt { prompt, outcome }))
    }

    pub fn pending_prompts(&self) -> Result<Vec<SecretPrompt>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, node_id, command_id, challenge, requester, purpose, scope, env_var,
                    created_at_ms, expires_at_ms
             FROM prompts WHERE resolved_at_ms IS NULL AND expires_at_ms > ?1
             ORDER BY created_at_ms",
        )?;
        let rows = statement.query_map(params![now_ms() as i64], |row| {
            Ok(SecretPrompt {
                id: row.get(0)?,
                node_id: row.get(1)?,
                command_id: row.get(2)?,
                challenge: row.get(3)?,
                requester: row.get(4)?,
                purpose: row.get(5)?,
                scope: row.get(6)?,
                env_var: row.get(7)?,
                created_at_ms: row.get::<_, i64>(8)? as u64,
                expires_at_ms: row.get::<_, i64>(9)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mark a prompt resolved exactly once. The first outcome wins.
    pub fn resolve_prompt(&self, id: &str, outcome: &str, now: u64) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE prompts SET outcome = ?1, resolved_at_ms = ?2
             WHERE id = ?3 AND resolved_at_ms IS NULL",
            params![outcome, now as i64, id],
        )?;
        Ok(changed > 0)
    }

    pub fn audit(
        &self,
        id: &str,
        at_ms: u64,
        actor: &str,
        kind: &str,
        detail: &Value,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO audit(id, at_ms, actor, kind, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                at_ms as i64,
                actor,
                kind,
                serde_json::to_string(detail)?
            ],
        )?;
        Ok(())
    }

    pub fn audit_entries(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, at_ms, actor, kind, detail FROM audit ORDER BY at_ms DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit], |row| {
            let detail: String = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                detail,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (id, at_ms, actor, kind, detail) = row?;
            entries.push(AuditEntry {
                id,
                at_ms: at_ms as u64,
                actor,
                kind,
                detail: serde_json::from_str(&detail).unwrap_or(Value::Null),
            });
        }
        Ok(entries)
    }
}

fn node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<NodeSummary>> {
    let capabilities: String = row.get(6)?;
    let labels: String = row.get(7)?;
    Ok(Ok(NodeSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        hostname: row.get(2)?,
        os: row.get(3)?,
        arch: row.get(4)?,
        agent_version: row.get(5)?,
        capabilities: serde_json::from_str(&capabilities)
            .unwrap_or_else(|_| Capabilities::baseline()),
        labels: serde_json::from_str(&labels).unwrap_or_default(),
        online: row.get::<_, i64>(8)? != 0,
        last_seen_ms: row.get::<_, i64>(9)? as u64,
    }))
}

fn command_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<CommandRecord>> {
    let argv: String = row.get(2)?;
    let status: String = row.get(4)?;
    Ok((|| {
        Ok(CommandRecord {
            id: row.get(0)?,
            node_id: row.get(1)?,
            argv: serde_json::from_str(&argv)?,
            cwd: row.get(3)?,
            status: CommandStatus::parse(&status)
                .with_context(|| format!("unknown command status {status}"))?,
            idempotency_key: row.get(5)?,
            created_at_ms: row.get::<_, i64>(6)? as u64,
            started_at_ms: row.get::<_, Option<i64>>(7)?.map(|value| value as u64),
            finished_at_ms: row.get::<_, Option<i64>>(8)?.map(|value| value as u64),
            exit_code: row.get(9)?,
            message: row.get(10)?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_protocol::{CommandRequest, SecretSpec};

    fn request(key: &str) -> CommandRequest {
        CommandRequest {
            id: "unused".into(),
            idempotency_key: key.into(),
            argv: vec!["uptime".into()],
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: 30_000,
            secret: None,
        }
    }

    #[test]
    fn enrollment_codes_are_single_use() {
        let db = Db::open_memory().expect("db");
        db.create_enrollment("hash", PrincipalKind::Node, "forge", 1_000)
            .expect("enroll");
        let first = db
            .consume_enrollment("hash", 500)
            .expect("consume")
            .expect("valid");
        assert_eq!(first.name, "forge");
        assert!(
            db.consume_enrollment("hash", 500)
                .expect("consume")
                .is_none()
        );
    }

    #[test]
    fn expired_enrollment_is_refused() {
        let db = Db::open_memory().expect("db");
        db.create_enrollment("hash", PrincipalKind::Device, "laptop", 1_000)
            .expect("enroll");
        assert!(
            db.consume_enrollment("hash", 1_000)
                .expect("consume")
                .is_none()
        );
    }

    #[test]
    fn idempotency_returns_the_first_command() {
        let db = Db::open_memory().expect("db");
        let first = request("same");
        let (record, created) = db.create_command("n-1", "c-1", &first, 10).expect("insert");
        assert!(created);
        assert_eq!(record.id, "c-1");

        let second = request("same");
        let (again, created) = db
            .create_command("n-1", "c-2", &second, 20)
            .expect("insert");
        assert!(!created);
        assert_eq!(again.id, "c-1");
    }

    #[test]
    fn terminal_status_is_not_overwritten() {
        let db = Db::open_memory().expect("db");
        let (record, _) = db
            .create_command("n-1", "c-1", &request("key"), 10)
            .expect("insert");
        assert_eq!(record.status, CommandStatus::Queued);
        db.finish_command("c-1", CommandStatus::Succeeded, Some(0), None, 20)
            .expect("finish");
        let after = db
            .finish_command("c-1", CommandStatus::Failed, Some(1), Some("late"), 30)
            .expect("finish")
            .expect("record");
        assert_eq!(after.status, CommandStatus::Succeeded);
    }

    #[test]
    fn events_replay_after_a_sequence() {
        let db = Db::open_memory().expect("db");
        for seq in 1..=3 {
            db.insert_command_event("c-1", seq, StreamChannel::Stdout, "aGk=", seq)
                .expect("event");
        }
        let events = db.command_events("c-1", 1, 10).expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.seq, 2);
    }

    #[test]
    fn prompt_resolution_is_single_use() {
        let db = Db::open_memory().expect("db");
        let prompt = SecretPrompt {
            id: "p-1".into(),
            node_id: "n-1".into(),
            command_id: "c-1".into(),
            challenge: "x".repeat(43),
            requester: "node".into(),
            purpose: "sudo".into(),
            scope: "command".into(),
            env_var: "SUDO_PASSWORD".into(),
            created_at_ms: 1,
            expires_at_ms: 10_000,
        };
        db.insert_prompt(&prompt).expect("insert");
        assert!(db.resolve_prompt("p-1", "provided", 2).expect("resolve"));
        assert!(!db.resolve_prompt("p-1", "expired", 3).expect("resolve"));
        assert_eq!(
            db.prompt("p-1")
                .expect("get")
                .expect("exists")
                .outcome
                .as_deref(),
            Some("provided")
        );
    }

    #[test]
    fn restart_closes_unresolved_prompts() {
        let dir = std::env::temp_dir().join(format!("hermes-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("hub.db");
        let now = now_ms();
        {
            let db = Db::open(&path).expect("db");
            db.insert_prompt(&SecretPrompt {
                id: "p-restart".into(),
                node_id: "n-1".into(),
                command_id: "c-1".into(),
                challenge: "x".repeat(43),
                requester: "node".into(),
                purpose: "sudo".into(),
                scope: "command".into(),
                env_var: "SUDO_PASSWORD".into(),
                created_at_ms: now,
                expires_at_ms: now + 60_000,
            })
            .expect("insert");
            assert_eq!(db.pending_prompts().expect("pending").len(), 1);
        }
        let db = Db::open(&path).expect("reopen");
        assert!(db.pending_prompts().expect("pending").is_empty());
        assert_eq!(
            db.prompt("p-restart")
                .expect("get")
                .expect("exists")
                .outcome
                .as_deref(),
            Some("hub_restarted")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn node_disconnect_fails_unfinished_commands() {
        let db = Db::open_memory().expect("db");
        db.create_command("n-1", "c-1", &request("one"), 10)
            .expect("insert");
        db.create_command("n-1", "c-2", &request("two"), 10)
            .expect("insert");
        db.finish_command("c-2", CommandStatus::Succeeded, Some(0), None, 11)
            .expect("finish");
        let failed = db
            .fail_commands_for_node("n-1", "node disconnected", 12)
            .expect("fail");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, "c-1");
        assert_eq!(failed[0].status, CommandStatus::Failed);
    }

    #[test]
    fn command_with_secret_still_has_no_secret_column() {
        let db = Db::open_memory().expect("db");
        let mut request = request("secret");
        request.secret = Some(SecretSpec {
            env_var: "PGPASSWORD".into(),
            purpose: "backup".into(),
            scope: "one run".into(),
            timeout_ms: 60_000,
        });
        let (record, _) = db
            .create_command("n-1", "c-1", &request, 10)
            .expect("insert");
        let json = serde_json::to_string(&record).expect("json");
        assert!(!json.contains("PGPASSWORD"));
    }
}
