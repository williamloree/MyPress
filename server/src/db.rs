use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use mypress_proto::api::{MailboxRecord, Record, StateBlob};

pub const PULL_PAGE: i64 = 500;
/// Nombre max d'éléments conservés par groupe (= plus grande taille d'historique proposée).
pub const MAX_LIVE: i64 = 1000;
/// Durée de conservation des marqueurs de suppression.
pub const TOMBSTONE_TTL_MS: i64 = 60 * 24 * 3600 * 1000;

pub struct Db {
    conn: Connection,
}

pub enum GroupAccess {
    Ok,
    Denied,
    /// Groupe remplacé après un renouvellement de clé : seul le message de renouvellement reste lisible.
    Frozen,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS groups (
                 id TEXT PRIMARY KEY,
                 secret_hash BLOB NOT NULL,
                 created_at INTEGER NOT NULL,
                 frozen INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS records (
                 group_id TEXT NOT NULL,
                 id TEXT NOT NULL,
                 seq INTEGER NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 blob TEXT,
                 PRIMARY KEY (group_id, id)
             );
             CREATE INDEX IF NOT EXISTS records_seq ON records(group_id, seq);
             CREATE TABLE IF NOT EXISTS states (group_id TEXT PRIMARY KEY, version INTEGER NOT NULL, blob TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS rekeys (group_id TEXT PRIMARY KEY, body TEXT NOT NULL);",
        )?;
        Ok(Self { conn })
    }

    /// Vérifie le secret ; le premier appareil qui utilise un identifiant de groupe le crée.
    pub fn access(&self, group: &str, secret_hash: &[u8]) -> rusqlite::Result<GroupAccess> {
        let row: Option<(Vec<u8>, i64)> = self
            .conn
            .query_row("SELECT secret_hash, frozen FROM groups WHERE id = ?1", params![group], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        Ok(match row {
            None => {
                self.conn.execute(
                    "INSERT INTO groups (id, secret_hash, created_at) VALUES (?1, ?2, ?3)",
                    params![group, secret_hash, now_ms()],
                )?;
                GroupAccess::Ok
            }
            Some((h, _)) if h != secret_hash => GroupAccess::Denied,
            Some((_, 1)) => GroupAccess::Frozen,
            Some(_) => GroupAccess::Ok,
        })
    }

    /// Insère ou met à jour les éléments. Une suppression l'emporte toujours sur le contenu.
    pub fn push(&mut self, group: &str, records: &[Record]) -> rusqlite::Result<i64> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        for r in records {
            let existing: Option<bool> = tx
                .query_row(
                    "SELECT blob IS NULL FROM records WHERE group_id = ?1 AND id = ?2",
                    params![group, r.id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            let changes = match existing {
                None => true,
                Some(false) => r.blob.is_none(),
                Some(true) => false,
            };
            if !changes {
                continue;
            }
            let seq: i64 = tx.query_row(
                "INSERT INTO meta (key, value) VALUES ('seq', 1)
                 ON CONFLICT(key) DO UPDATE SET value = value + 1 RETURNING value",
                [],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO records (group_id, id, seq, created_at, updated_at, blob) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(group_id, id) DO UPDATE SET seq = ?3, updated_at = ?5, blob = ?6",
                params![group, r.id.to_string(), seq, r.created_at, now, r.blob],
            )?;
        }
        // Au-delà de la limite, les plus anciens sont effacés (les appareils font de même).
        tx.execute(
            "DELETE FROM records WHERE group_id = ?1 AND blob IS NOT NULL AND id IN (
                 SELECT id FROM records WHERE group_id = ?1 AND blob IS NOT NULL
                 ORDER BY created_at DESC, id DESC LIMIT -1 OFFSET ?2)",
            params![group, MAX_LIVE],
        )?;
        tx.execute(
            "DELETE FROM records WHERE group_id = ?1 AND blob IS NULL AND updated_at < ?2",
            params![group, now - TOMBSTONE_TTL_MS],
        )?;
        tx.commit()?;
        self.max_seq(group)
    }

    pub fn max_seq(&self, group: &str) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM records WHERE group_id = ?1",
            params![group],
            |r| r.get(0),
        )
    }

    pub fn pull(&self, group: &str, since: i64) -> rusqlite::Result<(Vec<MailboxRecord>, bool)> {
        let mut stmt = self.conn.prepare(
            "SELECT id, seq, created_at, blob FROM records WHERE group_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![group, since, PULL_PAGE + 1], |r| {
            let id: String = r.get(0)?;
            Ok(MailboxRecord {
                seq: r.get(1)?,
                record: Record { id: Uuid::parse_str(&id).unwrap_or_default(), created_at: r.get(2)?, blob: r.get(3)? },
            })
        })?;
        let mut out: Vec<MailboxRecord> = rows.collect::<Result<_, _>>()?;
        let more = out.len() as i64 > PULL_PAGE;
        out.truncate(PULL_PAGE as usize);
        Ok((out, more))
    }

    /// Conserve l'état de version la plus élevée. Retourne `true` s'il a été remplacé.
    pub fn put_state(&self, group: &str, state: &StateBlob) -> rusqlite::Result<bool> {
        let n = self.conn.execute(
            "INSERT INTO states (group_id, version, blob) VALUES (?1, ?2, ?3)
             ON CONFLICT(group_id) DO UPDATE SET version = ?2, blob = ?3 WHERE version < ?2",
            params![group, state.version as i64, state.blob],
        )?;
        Ok(n > 0)
    }

    pub fn state(&self, group: &str) -> rusqlite::Result<Option<StateBlob>> {
        self.conn
            .query_row("SELECT version, blob FROM states WHERE group_id = ?1", params![group], |r| {
                Ok(StateBlob { version: r.get::<_, i64>(0)? as u64, blob: r.get(1)? })
            })
            .optional()
    }

    /// Fige le groupe : contenu et état sont effacés, seule la nouvelle clé (chiffrée par appareil) reste.
    pub fn rekey(&self, group: &str, body: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO rekeys (group_id, body) VALUES (?1, ?2) ON CONFLICT(group_id) DO UPDATE SET body = ?2",
            params![group, body],
        )?;
        self.conn.execute("DELETE FROM records WHERE group_id = ?1", params![group])?;
        self.conn.execute("DELETE FROM states WHERE group_id = ?1", params![group])?;
        self.conn.execute("UPDATE groups SET frozen = 1 WHERE id = ?1", params![group])?;
        Ok(())
    }

    pub fn get_rekey(&self, group: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT body FROM rekeys WHERE group_id = ?1", params![group], |r| r.get(0))
            .optional()
    }
}
