//! SQLite persistence for the trusted backend.
//!
//! Only token hashes are stored. A database leak does not yield usable device
//! credentials. Pairing codes are hashed and single-use as well.

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::protocol::{
    Capabilities, CommandKind, CommandRequest, CommandResult, DeviceSnapshot,
    MAX_COMMAND_TTL_SECONDS, PROTOCOL_VERSION, PairingCode,
};
use crate::security::{constant_time_eq, hash_secret, new_device_token, new_pairing_code};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("that pairing code is invalid or has already been used")]
    InvalidPairingCode,
    #[error("that pairing code has expired")]
    ExpiredPairingCode,
    #[error("unknown device or credential")]
    Unauthorized,
    #[error("device not found")]
    NotFound,
    #[error("command not found or already completed")]
    CommandNotFound,
    #[error("storage error: {0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub owner_discord_id: String,
    pub token_hash: String,
    pub capabilities: Capabilities,
    pub snapshot: Option<DeviceSnapshot>,
    pub online: bool,
    pub last_seen: Option<DateTime<Utc>>,
    pub agent_version: Option<String>,
    pub platform: Option<String>,
}

impl DeviceRecord {
    pub fn public(&self) -> crate::protocol::DevicePublic {
        crate::protocol::DevicePublic {
            id: self.id.clone(),
            name: self.name.clone(),
            owner_discord_id: self.owner_discord_id.clone(),
            online: self.online,
            last_seen: self.last_seen,
            agent_version: self.agent_version.clone(),
            platform: self.platform.clone(),
            session_count: self
                .snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.sessions.len()),
            capabilities: self.capabilities.clone(),
        }
    }

    pub fn sessions(&self) -> &[crate::protocol::SessionSnapshot] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.sessions.as_slice())
    }
}

#[derive(Debug, Clone)]
pub struct CompletedCommand {
    pub command_id: String,
    pub device_id: String,
    pub kind: CommandKind,
    pub result: CommandResult,
    pub interaction_token: Option<String>,
    pub application_id: Option<String>,
    pub channel_id: Option<String>,
}

/// One audit lifecycle row. Every stage is explicit. `submitted` means the game
/// client accepted the request; `verified` means a reliable event confirmed the
/// outcome. Neither is inferred from Discord accepting the interaction.
#[derive(Debug, Clone)]
pub struct AuditRow {
    pub id: String,
    pub command_id: Option<String>,
    pub requester_discord_id: String,
    pub device_id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub payload_summary: String,
    pub created_at: DateTime<Utc>,
    pub authorized: bool,
    pub authorization_error: Option<String>,
    pub confirmation_required: bool,
    pub confirmation_obtained: bool,
    pub agent_received_at: Option<DateTime<Utc>>,
    pub bridge_accepted: Option<bool>,
    pub bridge_error: Option<String>,
    pub submitted: Option<bool>,
    pub verified: Option<bool>,
    pub outcome: String,
    pub detail: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub requester: Option<String>,
    pub device_id: Option<String>,
    pub session_id: Option<String>,
    pub outcome: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn open(url: &str) -> Result<Self, StoreError> {
        let memory = url.contains(":memory:");
        let mut options = SqliteConnectOptions::from_str(url)?;
        options = options
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        if !memory {
            options = options
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal);
        }
        let pool = SqlitePoolOptions::new()
            .max_connections(if memory { 1 } else { 8 })
            .connect_with(options)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<(), StoreError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS devices (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                owner_discord_id TEXT NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                capabilities_json TEXT NOT NULL,
                snapshot_json TEXT,
                online INTEGER NOT NULL DEFAULT 0,
                last_seen TEXT,
                agent_version TEXT,
                platform TEXT,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS pair_codes (
                code_hash TEXT PRIMARY KEY,
                owner_discord_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                used_at TEXT
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS selections (
                discord_user_id TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                device_id TEXT,
                session_id TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (discord_user_id, channel_id)
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS commands (
                id TEXT PRIMARY KEY,
                device_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                session_id TEXT,
                payload_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                delivered_at TEXT,
                completed_at TEXT,
                result_json TEXT,
                interaction_token TEXT,
                application_id TEXT,
                channel_id TEXT,
                FOREIGN KEY(device_id) REFERENCES devices(id) ON DELETE CASCADE
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS commands_pending ON commands(device_id, delivered_at, expires_at)")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS command_audit (
                id TEXT PRIMARY KEY,
                command_id TEXT,
                requester_discord_id TEXT NOT NULL,
                device_id TEXT NOT NULL,
                session_id TEXT,
                kind TEXT NOT NULL,
                payload_summary TEXT NOT NULL,
                created_at TEXT NOT NULL,
                authorized INTEGER NOT NULL,
                authorization_error TEXT,
                confirmation_required INTEGER NOT NULL DEFAULT 0,
                confirmation_obtained INTEGER NOT NULL DEFAULT 0,
                agent_received_at TEXT,
                bridge_accepted INTEGER,
                bridge_error TEXT,
                submitted INTEGER,
                verified INTEGER,
                outcome TEXT NOT NULL,
                detail TEXT,
                completed_at TEXT,
                FOREIGN KEY(device_id) REFERENCES devices(id) ON DELETE CASCADE
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS command_audit_owner ON command_audit(requester_discord_id, created_at)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS command_audit_device ON command_audit(device_id, created_at)",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn create_pairing_code(
        &self,
        owner_discord_id: &str,
        ttl_seconds: i64,
    ) -> Result<PairingCode, StoreError> {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(ttl_seconds.clamp(60, 3600));
        let code = new_pairing_code().map_err(StoreError::Other)?;
        let normalized = crate::security::normalize_pairing_code(&code);
        sqlx::query(
            "INSERT INTO pair_codes (code_hash, owner_discord_id, created_at, expires_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(hash_secret(&normalized))
        .bind(owner_discord_id)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(PairingCode {
            code,
            owner_discord_id: owner_discord_id.to_string(),
            expires_at,
        })
    }

    pub async fn claim_pairing_code(
        &self,
        code: &str,
        device_name: &str,
        platform: &str,
        capabilities: &Capabilities,
    ) -> Result<(String, String), StoreError> {
        let normalized = crate::security::normalize_pairing_code(code);
        let hash = hash_secret(&normalized);
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT owner_discord_id, expires_at, used_at FROM pair_codes WHERE code_hash = ?",
        )
        .bind(&hash)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::InvalidPairingCode)?;

        let used_at: Option<String> = row.try_get("used_at")?;
        if used_at.is_some() {
            return Err(StoreError::InvalidPairingCode);
        }
        let expires_at: String = row.try_get("expires_at")?;
        let expires_at = DateTime::parse_from_rfc3339(&expires_at)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|_| StoreError::ExpiredPairingCode)?;
        if Utc::now() >= expires_at {
            return Err(StoreError::ExpiredPairingCode);
        }

        let updated = sqlx::query(
            "UPDATE pair_codes SET used_at = ? WHERE code_hash = ? AND used_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::InvalidPairingCode);
        }

        let owner: String = row.try_get("owner_discord_id")?;
        let device_id = Uuid::new_v4().to_string();
        let token = new_device_token().map_err(StoreError::Other)?;
        let capabilities_json = serde_json::to_string(capabilities)
            .map_err(|err| StoreError::Other(err.to_string()))?;
        sqlx::query(
            "INSERT INTO devices
             (id, name, owner_discord_id, token_hash, capabilities_json, platform, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&device_id)
        .bind(device_name)
        .bind(owner)
        .bind(hash_secret(&token))
        .bind(capabilities_json)
        .bind(platform)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((device_id, token))
    }

    pub async fn authenticate(&self, token: &str) -> Result<DeviceRecord, StoreError> {
        let hash = hash_secret(token);
        let row = sqlx::query("SELECT * FROM devices WHERE token_hash = ?")
            .bind(&hash)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::Unauthorized)?;
        let stored: String = row.try_get("token_hash")?;
        if !constant_time_eq(&stored, &hash) {
            return Err(StoreError::Unauthorized);
        }
        row_to_device(&row)
    }

    pub async fn get_device(&self, id: &str) -> Result<DeviceRecord, StoreError> {
        let row = sqlx::query("SELECT * FROM devices WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        row_to_device(&row)
    }

    pub async fn list_devices(&self, owner: &str) -> Result<Vec<DeviceRecord>, StoreError> {
        let rows = sqlx::query("SELECT * FROM devices WHERE owner_discord_id = ? ORDER BY name")
            .bind(owner)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_device).collect()
    }

    pub async fn list_all_devices(&self) -> Result<Vec<DeviceRecord>, StoreError> {
        let rows = sqlx::query("SELECT * FROM devices ORDER BY owner_discord_id, name")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_device).collect()
    }

    pub async fn record_heartbeat(
        &self,
        device_id: &str,
        snapshot: &DeviceSnapshot,
    ) -> Result<(), StoreError> {
        let json =
            serde_json::to_string(snapshot).map_err(|err| StoreError::Other(err.to_string()))?;
        let capabilities = serde_json::to_string(&snapshot.capabilities)
            .map_err(|err| StoreError::Other(err.to_string()))?;
        sqlx::query(
            "UPDATE devices SET snapshot_json = ?, capabilities_json = ?, online = 1,
             last_seen = ?, agent_version = ?, platform = ? WHERE id = ?",
        )
        .bind(json)
        .bind(capabilities)
        .bind(Utc::now().to_rfc3339())
        .bind(&snapshot.agent_version)
        .bind(&snapshot.platform)
        .bind(device_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_offline(&self, device_id: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE devices SET online = 0 WHERE id = ?")
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Mark devices that stopped heartbeating as offline. Returns only the
    /// devices whose state actually transitioned, so the alert loop does not
    /// repeat an offline notification.
    pub async fn mark_stale_offline(
        &self,
        stale_after: Duration,
    ) -> Result<Vec<DeviceRecord>, StoreError> {
        let cutoff = Utc::now() - chrono::Duration::from_std(stale_after).unwrap_or_default();
        let rows = sqlx::query(
            "SELECT * FROM devices WHERE online = 1 AND (last_seen IS NULL OR last_seen < ?)",
        )
        .bind(cutoff.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;
        let devices: Vec<DeviceRecord> =
            rows.iter().map(row_to_device).collect::<Result<_, _>>()?;
        for device in &devices {
            self.mark_offline(&device.id).await?;
        }
        Ok(devices)
    }

    pub async fn revoke_device(&self, device_id: &str) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM commands WHERE device_id = ?")
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM selections WHERE device_id = ?")
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        let deleted = sqlx::query("DELETE FROM devices WHERE id = ?")
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn channel_selection(
        &self,
        user_id: &str,
        channel_id: &str,
    ) -> Result<Option<(Option<String>, Option<String>)>, StoreError> {
        let row = sqlx::query(
            "SELECT device_id, session_id FROM selections WHERE discord_user_id = ? AND channel_id = ?",
        )
        .bind(user_id)
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| (row.get("device_id"), row.get("session_id"))))
    }

    pub async fn set_selection(
        &self,
        user_id: &str,
        channel_id: &str,
        device_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO selections (discord_user_id, channel_id, device_id, session_id, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(discord_user_id, channel_id) DO UPDATE SET
               device_id = excluded.device_id,
               session_id = excluded.session_id,
               updated_at = excluded.updated_at",
        )
        .bind(user_id)
        .bind(channel_id)
        .bind(device_id)
        .bind(session_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn enqueue_command(
        &self,
        device_id: &str,
        kind: CommandKind,
        session_id: Option<String>,
        payload: Value,
        payload_summary: &str,
        requester_discord_id: &str,
        confirmation_required: bool,
        confirmation_obtained: bool,
        interaction_token: Option<String>,
        application_id: Option<String>,
        channel_id: Option<String>,
    ) -> Result<CommandRequest, StoreError> {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(MAX_COMMAND_TTL_SECONDS);
        let id = Uuid::new_v4().to_string();
        let payload_json =
            serde_json::to_string(&payload).map_err(|err| StoreError::Other(err.to_string()))?;
        let summary = audit_text(payload_summary, 240);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO commands
             (id, device_id, kind, session_id, payload_json, created_at, expires_at,
              interaction_token, application_id, channel_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(device_id)
        .bind(kind.as_str())
        .bind(&session_id)
        .bind(payload_json)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .bind(&interaction_token)
        .bind(&application_id)
        .bind(&channel_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO command_audit
             (id, command_id, requester_discord_id, device_id, session_id, kind,
              payload_summary, created_at, authorized, confirmation_required,
              confirmation_obtained, outcome)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, 'queued')",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&id)
        .bind(requester_discord_id)
        .bind(device_id)
        .bind(&session_id)
        .bind(kind.as_str())
        .bind(summary)
        .bind(now.to_rfc3339())
        .bind(i64::from(confirmation_required))
        .bind(i64::from(confirmation_obtained))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(CommandRequest {
            id,
            kind,
            session_id,
            payload,
            created_at: now,
            expires_at,
        })
    }

    /// Record a request that was refused before it could be queued. This keeps
    /// policy denials visible in `/command-logs` without creating a command the
    /// agent could ever claim.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_denied_audit(
        &self,
        requester_discord_id: &str,
        device_id: &str,
        session_id: Option<&str>,
        kind: &str,
        payload_summary: &str,
        confirmation_required: bool,
        reason: &str,
    ) -> Result<(), StoreError> {
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO command_audit
             (id, command_id, requester_discord_id, device_id, session_id, kind,
              payload_summary, created_at, authorized, authorization_error,
              confirmation_required, confirmation_obtained, submitted, verified,
              outcome, detail, completed_at)
             VALUES (?, NULL, ?, ?, ?, ?, ?, ?, 0, ?, ?, 0, 0, 0, 'denied', ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(requester_discord_id)
        .bind(device_id)
        .bind(session_id)
        .bind(audit_text(kind, 32))
        .bind(audit_text(payload_summary, 240))
        .bind(now.to_rfc3339())
        .bind(audit_text(reason, 512))
        .bind(i64::from(confirmation_required))
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark the audit rows for commands that were just handed to the agent.
    pub async fn mark_audit_agent_received(
        &self,
        device_id: &str,
        command_ids: &[String],
    ) -> Result<(), StoreError> {
        if command_ids.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        for id in command_ids {
            sqlx::query(
                "UPDATE command_audit SET agent_received_at = ?, outcome = 'agent_received'
                 WHERE command_id = ? AND device_id = ? AND agent_received_at IS NULL",
            )
            .bind(&now)
            .bind(id)
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn claim_pending_commands(
        &self,
        device_id: &str,
        limit: usize,
    ) -> Result<Vec<CommandRequest>, StoreError> {
        let now = Utc::now();
        sqlx::query("DELETE FROM commands WHERE expires_at < ? AND delivered_at IS NULL")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await?;
        let rows = sqlx::query(
            "SELECT * FROM commands WHERE device_id = ? AND delivered_at IS NULL AND expires_at > ?
             ORDER BY created_at LIMIT ?",
        )
        .bind(device_id)
        .bind(now.to_rfc3339())
        .bind(limit.clamp(1, 16) as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::new();
        for row in rows {
            let id: String = row.try_get("id")?;
            // The agent is about to receive the payload. Clear the stored copy
            // at the same time so a chat line or command argument is not left
            // at rest after delivery; the row keeps only its lifecycle.
            let updated = sqlx::query(
                "UPDATE commands SET delivered_at = ?, payload_json = '{}'
                 WHERE id = ? AND delivered_at IS NULL",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(&id)
            .execute(&self.pool)
            .await?;
            if updated.rows_affected() != 1 {
                continue;
            }
            let kind: String = row.try_get("kind")?;
            let kind = parse_kind(&kind)?;
            let payload: String = row.try_get("payload_json")?;
            out.push(CommandRequest {
                id,
                kind,
                session_id: row.get("session_id"),
                payload: serde_json::from_str(&payload)
                    .map_err(|err| StoreError::Other(err.to_string()))?,
                created_at: parse_time_opt(row.get("created_at")).unwrap_or(now),
                expires_at: parse_time_opt(row.get("expires_at")).unwrap_or(now),
            });
        }
        Ok(out)
    }

    pub async fn complete_command(
        &self,
        device_id: &str,
        result: &CommandResult,
    ) -> Result<Option<CompletedCommand>, StoreError> {
        let row = sqlx::query(
            "SELECT * FROM commands WHERE id = ? AND device_id = ? AND completed_at IS NULL",
        )
        .bind(&result.command_id)
        .bind(device_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::CommandNotFound)?;
        // A screenshot is a transient delivery payload. Persisting several
        // megabytes of image data in the command log would be an unnecessary
        // retention risk, so only the text result is stored. Chat history is
        // likewise delivered to Discord and not duplicated into the audit DB.
        let mut persisted = result.clone();
        persisted.screenshot = None;
        persisted.chat = None;
        let updated = sqlx::query(
            "UPDATE commands SET completed_at = ?, result_json = ?, payload_json = '{}'
             WHERE id = ? AND completed_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(
            serde_json::to_string(&persisted)
                .map_err(|err| StoreError::Other(err.to_string()))?,
        )
        .bind(&result.command_id)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Ok(None);
        }
        let kind: String = row.try_get("kind")?;
        let kind = parse_kind(&kind)?;
        let execution = result.execution.as_ref();
        let submitted = execution.map(|status| status.submitted);
        let verified = execution.map(|status| status.verified);
        let outcome = match execution {
            Some(status) if status.state == "rejected" => "rejected",
            Some(status) => status.state.as_str(),
            None if !result.success => "failed",
            None if kind.is_state_changing() => "unknown",
            None => "completed",
        };
        let bridge_error = execution
            .filter(|status| !status.submitted)
            .and_then(|status| status.detail.clone());
        sqlx::query(
            "UPDATE command_audit SET bridge_accepted = ?, bridge_error = ?, submitted = ?,
             verified = ?, outcome = ?, detail = ?, completed_at = ? WHERE command_id = ?",
        )
        .bind(submitted.map(i64::from))
        .bind(bridge_error.as_deref().map(|value| audit_text(value, 512)))
        .bind(submitted.map(i64::from))
        .bind(verified.map(i64::from))
        .bind(outcome)
        .bind(audit_text(&result.message, 512))
        .bind(Utc::now().to_rfc3339())
        .bind(&result.command_id)
        .execute(&self.pool)
        .await?;
        Ok(Some(CompletedCommand {
            command_id: result.command_id.clone(),
            device_id: device_id.to_string(),
            kind,
            result: result.clone(),
            interaction_token: row.get("interaction_token"),
            application_id: row.get("application_id"),
            channel_id: row.get("channel_id"),
        }))
    }

    /// Complete expired commands with an explicit failure result so a Discord
    /// deferred response never hangs forever when an agent is offline or
    /// crashes after the command was delivered.
    pub async fn expire_commands(&self) -> Result<Vec<CompletedCommand>, StoreError> {
        let now = Utc::now();
        let rows =
            sqlx::query("SELECT * FROM commands WHERE completed_at IS NULL AND expires_at < ?")
                .bind(now.to_rfc3339())
                .fetch_all(&self.pool)
                .await?;
        let mut expired = Vec::new();
        for row in rows {
            let id: String = row.try_get("id")?;
            let kind: String = row.try_get("kind")?;
            let result = CommandResult {
                command_id: id.clone(),
                success: false,
                message: "the request expired before the agent completed it".into(),
                log_excerpt: None,
                screenshot: None,
                chat: None,
                execution: None,
                completed_at: now,
            };
            let updated = sqlx::query(
                "UPDATE commands SET completed_at = ?, result_json = ?, payload_json = '{}'
                 WHERE id = ? AND completed_at IS NULL",
            )
            .bind(now.to_rfc3339())
            .bind(serde_json::to_string(&result).map_err(|err| StoreError::Other(err.to_string()))?)
            .bind(&id)
            .execute(&self.pool)
            .await?;
            if updated.rows_affected() != 1 {
                continue;
            }
            sqlx::query(
                "UPDATE command_audit SET outcome = 'expired', detail = ?, completed_at = ?
                 WHERE command_id = ?",
            )
            .bind("the request expired before the agent completed it")
            .bind(now.to_rfc3339())
            .bind(&id)
            .execute(&self.pool)
            .await?;
            expired.push(CompletedCommand {
                command_id: id,
                device_id: row.try_get("device_id")?,
                kind: parse_kind(&kind)?,
                result,
                interaction_token: row.get("interaction_token"),
                application_id: row.get("application_id"),
                channel_id: row.get("channel_id"),
            });
        }
        Ok(expired)
    }

    /// Read audit rows. Non-administrators are constrained to their own rows by
    /// the caller; `filter.requester` here is authoritative when set.
    pub async fn list_audit(&self, filter: &AuditFilter) -> Result<Vec<AuditRow>, StoreError> {
        let mut sql = String::from(
            "SELECT * FROM command_audit WHERE 1 = 1",
        );
        let mut conditions: Vec<(&str, String)> = Vec::new();
        if let Some(requester) = &filter.requester {
            conditions.push(("requester_discord_id = ?", requester.clone()));
        }
        if let Some(device_id) = &filter.device_id {
            conditions.push(("device_id = ?", device_id.clone()));
        }
        if let Some(session_id) = &filter.session_id {
            conditions.push(("session_id = ?", session_id.clone()));
        }
        if let Some(outcome) = &filter.outcome {
            conditions.push(("outcome = ?", outcome.clone()));
        }
        if let Some(since) = filter.since {
            conditions.push(("created_at >= ?", since.to_rfc3339()));
        }
        for (condition, _) in &conditions {
            sql.push_str(" AND ");
            sql.push_str(condition);
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ? OFFSET ?");
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for (_, value) in &conditions {
            query = query.bind(value);
        }
        let limit = filter.limit.clamp(1, 100) as i64;
        let offset = filter.offset.min(10_000) as i64;
        let rows = query.bind(limit).bind(offset).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_audit).collect()
    }

    /// Delete audit rows for one requester, optionally limited to one device.
    /// Returns the number of deleted rows.
    pub async fn clear_audit(
        &self,
        requester_discord_id: &str,
        device_id: Option<&str>,
    ) -> Result<u64, StoreError> {
        let deleted = match device_id {
            Some(device_id) => {
                sqlx::query(
                    "DELETE FROM command_audit WHERE requester_discord_id = ? AND device_id = ?",
                )
                .bind(requester_discord_id)
                .bind(device_id)
                .execute(&self.pool)
                .await?
            }
            None => {
                sqlx::query("DELETE FROM command_audit WHERE requester_discord_id = ?")
                    .bind(requester_discord_id)
                    .execute(&self.pool)
                    .await?
            }
        };
        Ok(deleted.rows_affected())
    }

    pub async fn cleanup(&self, audit_retention_days: i64) -> Result<(), StoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("DELETE FROM pair_codes WHERE expires_at < ? OR used_at IS NOT NULL")
            .bind(&now)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM commands WHERE completed_at IS NOT NULL AND completed_at < ?")
            .bind((Utc::now() - chrono::Duration::days(2)).to_rfc3339())
            .execute(&self.pool)
            .await?;
        let days = audit_retention_days.clamp(1, 90);
        sqlx::query("DELETE FROM command_audit WHERE created_at < ?")
            .bind((Utc::now() - chrono::Duration::days(days)).to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn row_to_device(row: &sqlx::sqlite::SqliteRow) -> Result<DeviceRecord, StoreError> {
    let capabilities_json: String = row.try_get("capabilities_json")?;
    let capabilities: Capabilities = serde_json::from_str(&capabilities_json).unwrap_or_default();
    let snapshot_json: Option<String> = row.try_get("snapshot_json")?;
    let snapshot = snapshot_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<DeviceSnapshot>(json).ok());
    Ok(DeviceRecord {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        owner_discord_id: row.try_get("owner_discord_id")?,
        token_hash: row.try_get("token_hash")?,
        capabilities,
        snapshot,
        online: row.try_get::<i64, _>("online")? != 0,
        last_seen: row
            .try_get::<Option<String>, _>("last_seen")?
            .as_deref()
            .and_then(parse_time),
        agent_version: row.try_get("agent_version")?,
        platform: row.try_get("platform")?,
    })
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn parse_time_opt(value: Option<String>) -> Option<DateTime<Utc>> {
    value.as_deref().and_then(parse_time)
}

fn parse_kind(value: &str) -> Result<CommandKind, StoreError> {
    match value {
        "logs" => Ok(CommandKind::GetLogs),
        "launch" => Ok(CommandKind::Launch),
        "stop" => Ok(CommandKind::Stop),
        "screenshot" => Ok(CommandKind::Screenshot),
        "status" => Ok(CommandKind::GetStatus),
        "mods" => Ok(CommandKind::GetMods),
        "coordinates" => Ok(CommandKind::Coordinates),
        "chat" => Ok(CommandKind::ChatLogs),
        "say" => Ok(CommandKind::Say),
        "minecraft_command" => Ok(CommandKind::MinecraftCommand),
        "baritone_status" => Ok(CommandKind::BaritoneStatus),
        "baritone_control" => Ok(CommandKind::BaritoneControl),
        "pause" => Ok(CommandKind::Pause),
        "resume" => Ok(CommandKind::Resume),
        other => Err(StoreError::Other(format!("unknown command kind {other}"))),
    }
}

fn row_to_audit(row: &sqlx::sqlite::SqliteRow) -> Result<AuditRow, StoreError> {
    Ok(AuditRow {
        id: row.try_get("id")?,
        command_id: row.try_get("command_id")?,
        requester_discord_id: row.try_get("requester_discord_id")?,
        device_id: row.try_get("device_id")?,
        session_id: row.try_get("session_id")?,
        kind: row.try_get("kind")?,
        payload_summary: row.try_get("payload_summary")?,
        created_at: parse_time_opt(row.get("created_at")).unwrap_or_else(Utc::now),
        authorized: row.try_get::<i64, _>("authorized")? != 0,
        authorization_error: row.try_get("authorization_error")?,
        confirmation_required: row.try_get::<i64, _>("confirmation_required")? != 0,
        confirmation_obtained: row.try_get::<i64, _>("confirmation_obtained")? != 0,
        agent_received_at: parse_time_opt(row.get("agent_received_at")),
        bridge_accepted: row.try_get::<Option<i64>, _>("bridge_accepted")?.map(|v| v != 0),
        bridge_error: row.try_get("bridge_error")?,
        submitted: row.try_get::<Option<i64>, _>("submitted")?.map(|v| v != 0),
        verified: row.try_get::<Option<i64>, _>("verified")?.map(|v| v != 0),
        outcome: row.try_get("outcome")?,
        detail: row.try_get("detail")?,
        completed_at: parse_time_opt(row.get("completed_at")),
    })
}

/// Audit text is bounded and stripped of control characters. Callers pass a
/// redacted summary; this is the last line of defence against a multi-line
/// payload or a secret-shaped value reaching the log UI.
fn audit_text(value: &str, max_chars: usize) -> String {
    let clean: String = value.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    crate::protocol::truncate_text(clean.trim(), max_chars * 4)
}

pub fn protocol_version() -> u32 {
    PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SystemSpecs;

    async fn store() -> Store {
        Store::open("sqlite::memory:").await.unwrap()
    }

    fn snapshot() -> DeviceSnapshot {
        DeviceSnapshot {
            protocol: PROTOCOL_VERSION,
            agent_version: "0.0.0-test".into(),
            platform: "test".into(),
            specs: SystemSpecs::default(),
            sessions: Vec::new(),
            capabilities: Capabilities::default(),
            captured_at: Utc::now(),
            agent_started_at: Utc::now(),
            log_excerpt_supported: true,
        }
    }

    #[tokio::test]
    async fn pairing_is_single_use_and_stores_only_a_hash() {
        let store = store().await;
        let code = store.create_pairing_code("owner-1", 600).await.unwrap();
        let (device_id, token) = store
            .claim_pairing_code(&code.code, "Gaming PC", "windows", &Capabilities::default())
            .await
            .unwrap();
        assert!(device_id.len() >= 32);
        assert!(token.len() >= 43);
        assert!(matches!(
            store
                .claim_pairing_code(&code.code, "again", "windows", &Capabilities::default())
                .await,
            Err(StoreError::InvalidPairingCode)
        ));
        let device = store.authenticate(&token).await.unwrap();
        assert_eq!(device.id, device_id);
        assert_eq!(device.name, "Gaming PC");
        assert!(!device.token_hash.contains(&token));
        assert!(store.authenticate("not-the-token").await.is_err());
    }

    #[tokio::test]
    async fn expired_pairing_code_is_rejected() {
        let store = store().await;
        let code = store.create_pairing_code("owner-1", 600).await.unwrap();
        let hash = hash_secret(&crate::security::normalize_pairing_code(&code.code));
        sqlx::query("UPDATE pair_codes SET expires_at = ? WHERE code_hash = ?")
            .bind((Utc::now() - chrono::Duration::seconds(1)).to_rfc3339())
            .bind(hash)
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(matches!(
            store
                .claim_pairing_code(&code.code, "PC", "linux", &Capabilities::default())
                .await,
            Err(StoreError::ExpiredPairingCode)
        ));
    }

    #[tokio::test]
    async fn heartbeat_and_selection_round_trip() {
        let store = store().await;
        let code = store.create_pairing_code("owner-1", 600).await.unwrap();
        let (device_id, token) = store
            .claim_pairing_code(&code.code, "PC", "windows", &Capabilities::default())
            .await
            .unwrap();
        store
            .record_heartbeat(&device_id, &snapshot())
            .await
            .unwrap();
        let device = store.authenticate(&token).await.unwrap();
        assert!(device.online);
        assert_eq!(device.sessions().len(), 0);
        store
            .set_selection("owner-1", "channel-1", Some(&device_id), None)
            .await
            .unwrap();
        let (selected, _) = store
            .channel_selection("owner-1", "channel-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(selected.as_deref(), Some(device_id.as_str()));
    }

    #[tokio::test]
    async fn revoked_device_loses_credential_and_commands() {
        let store = store().await;
        let code = store.create_pairing_code("owner-1", 600).await.unwrap();
        let (device_id, token) = store
            .claim_pairing_code(&code.code, "PC", "windows", &Capabilities::default())
            .await
            .unwrap();
        store.revoke_device(&device_id).await.unwrap();
        assert!(store.authenticate(&token).await.is_err());
        assert!(store.get_device(&device_id).await.is_err());
    }

    #[tokio::test]
    async fn command_is_delivered_once_and_completed_once() {
        let store = store().await;
        let code = store.create_pairing_code("owner-1", 600).await.unwrap();
        let (device_id, _token) = store
            .claim_pairing_code(&code.code, "PC", "windows", &Capabilities::default())
            .await
            .unwrap();
        let command = store
            .enqueue_command(
                &device_id,
                CommandKind::GetLogs,
                None,
                serde_json::json!({"lines": 50}),
                "logs",
                "owner-1",
                false,
                false,
                Some("interaction".into()),
                Some("app".into()),
                Some("channel".into()),
            )
            .await
            .unwrap();
        let first = store.claim_pending_commands(&device_id, 8).await.unwrap();
        assert_eq!(first.len(), 1);
        let second = store.claim_pending_commands(&device_id, 8).await.unwrap();
        assert!(second.is_empty());
        let result = CommandResult {
            command_id: command.id,
            success: true,
            message: "ok".into(),
            log_excerpt: Some("hello".into()),
            screenshot: None,
            chat: None,
            execution: None,
            completed_at: Utc::now(),
        };
        let completed = store
            .complete_command(&device_id, &result)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.interaction_token.as_deref(), Some("interaction"));
        assert!(store.complete_command(&device_id, &result).await.is_err());
    }

    #[tokio::test]
    async fn screenshots_are_relayed_but_never_persisted() {
        let store = store().await;
        let code = store.create_pairing_code("owner-1", 600).await.unwrap();
        let (device_id, _) = store
            .claim_pairing_code(&code.code, "PC", "windows", &Capabilities::default())
            .await
            .unwrap();
        let command = store
            .enqueue_command(
                &device_id,
                CommandKind::Screenshot,
                None,
                serde_json::json!({}),
                "screenshot",
                "owner-1",
                false,
                false,
                Some("interaction".into()),
                Some("app".into()),
                Some("channel".into()),
            )
            .await
            .unwrap();
        let result = CommandResult {
            screenshot: Some(crate::protocol::ScreenshotAttachment {
                content_type: "image/png".into(),
                filename: "minecraft.png".into(),
                width: 16,
                height: 16,
                data_base64: "iVBORw0KGgo=".into(),
            }),
            ..CommandResult::text(command.id, true, "captured", None)
        };
        let completed = store
            .complete_command(&device_id, &result)
            .await
            .unwrap()
            .unwrap();
        assert!(completed.result.screenshot.is_some());

        let stored: String = sqlx::query_scalar("SELECT result_json FROM commands")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert!(stored.contains("captured"));
        assert!(!stored.contains("data_base64"));
        assert!(!stored.contains("iVBORw0KGgo"));
    }

    #[tokio::test]
    async fn expired_command_completes_with_a_failure_instead_of_hanging() {
        let store = store().await;
        let code = store.create_pairing_code("owner-1", 600).await.unwrap();
        let (device_id, _) = store
            .claim_pairing_code(&code.code, "PC", "windows", &Capabilities::default())
            .await
            .unwrap();
        let command = store
            .enqueue_command(
                &device_id,
                CommandKind::GetLogs,
                None,
                serde_json::json!({"lines": 10}),
                "logs",
                "owner-1",
                false,
                false,
                Some("interaction".into()),
                Some("app".into()),
                Some("channel".into()),
            )
            .await
            .unwrap();
        sqlx::query("UPDATE commands SET expires_at = ? WHERE id = ?")
            .bind((Utc::now() - chrono::Duration::seconds(1)).to_rfc3339())
            .bind(&command.id)
            .execute(&store.pool)
            .await
            .unwrap();
        let expired = store.expire_commands().await.unwrap();
        assert_eq!(expired.len(), 1);
        assert!(!expired[0].result.success);
        assert_eq!(expired[0].interaction_token.as_deref(), Some("interaction"));
        assert!(store.expire_commands().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn command_audit_tracks_the_lifecycle_without_storing_chat_bodies() {
        let store = store().await;
        let code = store.create_pairing_code("owner-1", 600).await.unwrap();
        let (device_id, _) = store
            .claim_pairing_code(&code.code, "PC", "windows", &Capabilities::default())
            .await
            .unwrap();
        let command = store
            .enqueue_command(
                &device_id,
                CommandKind::Say,
                Some("session-1".into()),
                serde_json::json!({"message": "hello"}),
                "say request (5 characters)",
                "owner-1",
                false,
                true,
                Some("interaction".into()),
                Some("app".into()),
                Some("channel".into()),
            )
            .await
            .unwrap();

        let filter = || AuditFilter {
            requester: Some("owner-1".into()),
            device_id: None,
            session_id: None,
            outcome: None,
            since: None,
            limit: 50,
            offset: 0,
        };

        let queued = store.list_audit(&filter()).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].outcome, "queued");
        assert!(queued[0].authorized);
        assert!(!queued[0].confirmation_required);
        assert!(queued[0].confirmation_obtained);
        assert_eq!(queued[0].command_id.as_deref(), Some(command.id.as_str()));
        assert_eq!(queued[0].session_id.as_deref(), Some("session-1"));

        store
            .mark_audit_agent_received(&device_id, std::slice::from_ref(&command.id))
            .await
            .unwrap();
        let delivered = store.list_audit(&filter()).await.unwrap();
        assert_eq!(delivered[0].outcome, "agent_received");
        assert!(delivered[0].agent_received_at.is_some());

        store
            .claim_pending_commands(&device_id, 8)
            .await
            .unwrap();
        // Delivery is the end of the retained copy. A chat line must not sit in
        // the outbound queue after the agent has it.
        let stored_payload: String = sqlx::query_scalar("SELECT payload_json FROM commands WHERE id = ?")
            .bind(&command.id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(stored_payload, "{}");
        assert!(!stored_payload.contains("hello"));
        let marker = "chat-body-must-not-be-stored";
        let result = CommandResult {
            chat: Some(crate::protocol::ChatHistory {
                session_id: "session-1".into(),
                collection_enabled: true,
                collected_since: None,
                dropped: 0,
                messages: vec![crate::protocol::ChatMessage {
                    seq: 1,
                    captured_at: Utc::now(),
                    kind: "chat".into(),
                    sender: Some("Alex".into()),
                    text: marker.into(),
                }],
            }),
            execution: Some(crate::protocol::ExecutionStatus::submitted_unverified(
                "ClientPacketListener.sendChat accepted the line",
            )),
            ..CommandResult::text(
                command.id.clone(),
                true,
                "the message was submitted by the Minecraft client",
                None,
            )
        };
        let completed = store
            .complete_command(&device_id, &result)
            .await
            .unwrap()
            .unwrap();
        assert!(completed.result.chat.is_some());

        let done = store.list_audit(&filter()).await.unwrap();
        assert_eq!(done[0].outcome, "submitted_unverified");
        assert_eq!(done[0].submitted, Some(true));
        assert_eq!(done[0].verified, Some(false));
        assert!(
            done.iter().all(|row| {
                !row.payload_summary.contains(marker)
                    && row
                        .detail
                        .as_deref()
                        .is_none_or(|detail| !detail.contains(marker))
                    && row
                        .authorization_error
                        .as_deref()
                        .is_none_or(|detail| !detail.contains(marker))
            }),
            "chat bodies must never be persisted in the audit table"
        );
        let stored_result: String = sqlx::query_scalar("SELECT result_json FROM commands")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert!(!stored_result.contains(marker));

        store
            .record_denied_audit(
                "owner-1",
                &device_id,
                Some("session-1"),
                "minecraft_command",
                "mc-command op Alex",
                true,
                "the backend does not allow the administrative command category",
            )
            .await
            .unwrap();
        store
            .record_denied_audit("owner-2", &device_id, None, "chat", "chat history", false, "no")
            .await
            .unwrap();

        let denied = store
            .list_audit(&AuditFilter {
                requester: None,
                outcome: Some("denied".into()),
                ..filter()
            })
            .await
            .unwrap();
        assert_eq!(denied.len(), 2);
        let owner_denied = denied
            .iter()
            .find(|row| row.requester_discord_id == "owner-1")
            .unwrap();
        assert!(!owner_denied.authorized);
        assert!(owner_denied.confirmation_required);
        assert!(!owner_denied.confirmation_obtained);

        // Scoping: clearing one requester leaves the other row untouched.
        let deleted = store
            .clear_audit("owner-1", Some(&device_id))
            .await
            .unwrap();
        assert_eq!(deleted, 2);
        assert!(store.list_audit(&filter()).await.unwrap().is_empty());
        assert_eq!(
            store
                .list_audit(&AuditFilter {
                    requester: Some("owner-2".into()),
                    ..filter()
                })
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn stale_heartbeat_marks_only_transitioned_devices() {
        let store = store().await;
        let code = store.create_pairing_code("owner-1", 600).await.unwrap();
        let (device_id, _) = store
            .claim_pairing_code(&code.code, "PC", "windows", &Capabilities::default())
            .await
            .unwrap();
        store
            .record_heartbeat(&device_id, &snapshot())
            .await
            .unwrap();
        sqlx::query("UPDATE devices SET last_seen = ? WHERE id = ?")
            .bind((Utc::now() - chrono::Duration::seconds(120)).to_rfc3339())
            .bind(&device_id)
            .execute(&store.pool)
            .await
            .unwrap();
        let offline = store
            .mark_stale_offline(Duration::from_secs(45))
            .await
            .unwrap();
        assert_eq!(offline.len(), 1);
        assert!(
            store
                .mark_stale_offline(Duration::from_secs(45))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
