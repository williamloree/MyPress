//! Historique local (SQLite). Chaque appareil garde l'historique complet du groupe et
//! les marqueurs de suppression, ce qui permet de fusionner les historiques en P2P.
//! `synced` indique si la ligne a été envoyée à la boîte aux lettres.

use std::{collections::HashMap, path::Path};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use mypress_proto::api::{ContentKind, ItemPayload};

/// Durée de conservation des marqueurs de suppression.
const TOMBSTONE_TTL_MS: i64 = 60 * 24 * 3600 * 1000;

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: Uuid,
    pub created_at: i64,
    pub device_id: String,
    pub device_name: String,
    pub kind: &'static str,
    pub text: String,
}

/// Ligne brute : élément ou marqueur de suppression.
#[derive(Debug, Clone)]
pub struct Row {
    pub item: Item,
    pub deleted: bool,
}

#[derive(Debug, PartialEq)]
pub enum Applied {
    New,
    Deleted,
    Ignored,
}

pub struct Store {
    conn: Connection,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn hash_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

const COLS: &str = "id, created_at, device_id, device_name, kind, text, deleted";
/// Comme `COLS`, mais l'aperçu remplace le contenu des images (affichage de l'historique).
const VIEW_COLS: &str =
    "id, created_at, device_id, device_name, kind, CASE WHEN kind = 'image' THEN COALESCE(thumb, '') ELSE text END, deleted";

/// Minuscules sans accents, pour la recherche.
fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'ç' => 'c',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            c => c,
        })
        .collect()
}

fn thumb_of(kind: ContentKind, text: &str) -> Option<String> {
    (kind == ContentKind::Image).then(|| crate::clipboard::thumbnail(text)).flatten()
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    let id: String = r.get(0)?;
    let kind: String = r.get(4)?;
    Ok(Row {
        item: Item {
            id: Uuid::parse_str(&id).unwrap_or_default(),
            created_at: r.get(1)?,
            device_id: r.get(2)?,
            device_name: r.get(3)?,
            kind: ContentKind::parse(&kind).as_str(),
            text: r.get(5)?,
        },
        deleted: r.get::<_, i64>(6)? != 0,
    })
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS items (
                 id TEXT PRIMARY KEY,
                 created_at INTEGER NOT NULL,
                 device_id TEXT NOT NULL,
                 device_name TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 text TEXT NOT NULL,
                 hash TEXT NOT NULL,
                 deleted INTEGER NOT NULL DEFAULT 0,
                 updated_at INTEGER NOT NULL,
                 synced INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS items_live ON items(deleted, created_at);
             CREATE INDEX IF NOT EXISTS items_hash ON items(hash);",
        )?;
        // Aperçu des images, local à l'appareil. Échoue si la colonne existe déjà.
        let _ = conn.execute("ALTER TABLE items ADD COLUMN thumb TEXT", []);
        Ok(Self { conn })
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set(&self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        );
    }

    pub fn delete_kv(&self, key: &str) {
        let _ = self.conn.execute("DELETE FROM kv WHERE key = ?1", params![key]);
    }

    pub fn latest(&self) -> Option<Item> {
        self.conn
            .query_row(
                &format!("SELECT {COLS} FROM items WHERE deleted = 0 ORDER BY created_at DESC, id DESC LIMIT 1"),
                [],
                row,
            )
            .optional()
            .ok()
            .flatten()
            .map(|r| r.item)
    }

    pub fn row(&self, id: Uuid) -> Option<Row> {
        self.conn
            .query_row(&format!("SELECT {COLS} FROM items WHERE id = ?1"), params![id.to_string()], row)
            .optional()
            .ok()
            .flatten()
    }

    pub fn item(&self, id: Uuid) -> Option<Item> {
        self.row(id).filter(|r| !r.deleted).map(|r| r.item)
    }

    /// Copie locale. Si le même contenu existe déjà, l'ancien élément est supprimé
    /// pour que le contenu remonte en tête sans doublon. Retourne les lignes modifiées.
    pub fn capture(&mut self, text: &str, kind: ContentKind, device_id: &str, device_name: &str, limit: u32) -> rusqlite::Result<Vec<Uuid>> {
        let hash = hash_text(text);
        let dupes: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT id FROM items WHERE hash = ?1 AND deleted = 0")?;
            let rows = stmt.query_map(params![hash], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut changed = Vec::new();
        for id in dupes {
            let id = Uuid::parse_str(&id).unwrap_or_default();
            self.mark_deleted(id)?;
            changed.push(id);
        }
        let id = Uuid::now_v7();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO items (id, created_at, device_id, device_name, kind, text, hash, updated_at, thumb)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?2, ?8)",
            params![id.to_string(), now, device_id, device_name, kind.as_str(), text, hash, thumb_of(kind, text)],
        )?;
        changed.push(id);
        self.prune(limit)?;
        Ok(changed)
    }

    /// Applique un élément ou une suppression venant d'un autre appareil.
    /// Une suppression l'emporte toujours ; un élément connu n'est jamais réécrit.
    pub fn apply_remote(&mut self, id: Uuid, created_at: i64, payload: Option<&ItemPayload>, synced: bool) -> rusqlite::Result<Applied> {
        let existing = self.row(id);
        let now = now_ms();
        match (payload, existing) {
            (Some(_), Some(_)) => Ok(Applied::Ignored),
            (Some(p), None) => {
                self.conn.execute(
                    "INSERT INTO items (id, created_at, device_id, device_name, kind, text, hash, updated_at, synced, thumb)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        id.to_string(),
                        created_at,
                        p.device_id,
                        p.device_name,
                        p.kind.as_str(),
                        p.text,
                        hash_text(&p.text),
                        now,
                        synced,
                        thumb_of(p.kind, &p.text)
                    ],
                )?;
                Ok(Applied::New)
            }
            (None, Some(r)) if r.deleted => Ok(Applied::Ignored),
            (None, Some(_)) => {
                self.conn.execute(
                    "UPDATE items SET deleted = 1, text = '', hash = '', thumb = NULL, updated_at = ?2, synced = ?3 WHERE id = ?1",
                    params![id.to_string(), now, synced],
                )?;
                Ok(Applied::Deleted)
            }
            // Marqueur inconnu : conservé pour le relayer aux autres appareils.
            (None, None) => {
                self.conn.execute(
                    "INSERT INTO items (id, created_at, device_id, device_name, kind, text, hash, deleted, updated_at, synced)
                     VALUES (?1, ?2, '', '', 'text', '', '', 1, ?3, ?4)",
                    params![id.to_string(), created_at, now, synced],
                )?;
                Ok(Applied::Ignored)
            }
        }
    }

    /// Suppression locale : le texte est effacé tout de suite, le marqueur reste pour les autres appareils.
    pub fn mark_deleted(&self, id: Uuid) -> rusqlite::Result<bool> {
        let n = self.conn.execute(
            "UPDATE items SET deleted = 1, text = '', hash = '', thumb = NULL, updated_at = ?2, synced = 0 WHERE id = ?1 AND deleted = 0",
            params![id.to_string(), now_ms()],
        )?;
        Ok(n > 0)
    }

    pub fn clear(&self) -> rusqlite::Result<Vec<Uuid>> {
        let ids = self.live_ids()?;
        for id in &ids {
            self.mark_deleted(*id)?;
        }
        Ok(ids)
    }

    fn live_ids(&self) -> rusqlite::Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare("SELECT id FROM items WHERE deleted = 0")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).filter_map(|s| Uuid::parse_str(&s).ok()).collect())
    }

    /// Supprime définitivement les éléments au-delà de la limite (sans marqueur : chaque
    /// appareil applique la même limite) et les vieux marqueurs.
    pub fn prune(&self, limit: u32) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM items WHERE deleted = 0 AND id IN (
                 SELECT id FROM items WHERE deleted = 0
                 ORDER BY created_at DESC, id DESC LIMIT -1 OFFSET ?1)",
            params![limit],
        )?;
        self.conn.execute(
            "DELETE FROM items WHERE deleted = 1 AND updated_at < ?1",
            params![now_ms() - TOMBSTONE_TTL_MS],
        )?;
        Ok(())
    }

    /// Recherche insensible à la casse et aux accents, faite en Rust :
    /// l'historique est borné à quelques centaines d'éléments. Les images, sans texte,
    /// n'apparaissent que sans filtre, avec leur aperçu à la place du contenu.
    pub fn search(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {VIEW_COLS} FROM items WHERE deleted = 0 ORDER BY created_at DESC, id DESC"
        ))?;
        let needle = fold(query.trim());
        let mut out = Vec::new();
        for r in stmt.query_map([], row)? {
            let item = r?.item;
            if needle.is_empty() || (item.kind != "image" && fold(&item.text).contains(&needle)) {
                out.push(item);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Résumé envoyé aux autres appareils : identifiant → supprimé ?
    pub fn summary(&self) -> rusqlite::Result<HashMap<Uuid, bool>> {
        let mut stmt = self.conn.prepare("SELECT id, deleted FROM items")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)))?;
        Ok(rows.filter_map(|r| r.ok()).filter_map(|(id, d)| Uuid::parse_str(&id).ok().map(|id| (id, d))).collect())
    }

    /// Lignes qu'un autre appareil n'a pas, ou qu'il a encore alors qu'elles sont supprimées ici.
    pub fn missing_for(&self, peer: &HashMap<Uuid, bool>) -> rusqlite::Result<Vec<Row>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {COLS} FROM items"))?;
        let rows = stmt.query_map([], row)?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter(|r| match peer.get(&r.item.id) {
                None => true,
                Some(peer_deleted) => r.deleted && !peer_deleted,
            })
            .collect())
    }

    pub fn unsynced(&self, max: usize) -> rusqlite::Result<Vec<Row>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {COLS} FROM items WHERE synced = 0 LIMIT ?1"))?;
        let rows = stmt.query_map(params![max as i64], row)?;
        rows.collect()
    }

    pub fn mark_synced(&self, ids: &[Uuid]) -> rusqlite::Result<()> {
        for id in ids {
            self.conn.execute("UPDATE items SET synced = 1 WHERE id = ?1", params![id.to_string()])?;
        }
        Ok(())
    }

    /// Nouvelle boîte aux lettres ou nouvelle clé : tout est à renvoyer.
    pub fn mark_all_unsynced(&self) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE items SET synced = 0", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(text: &str) -> ItemPayload {
        ItemPayload { kind: ContentKind::Text, text: text.into(), device_name: "PC B".into(), device_id: "b".into() }
    }

    #[test]
    fn capture_dedupes_and_prunes() {
        let mut s = Store::open_in_memory().unwrap();
        s.capture("a", ContentKind::Text, "me", "PC", 50).unwrap();
        s.capture("b", ContentKind::Text, "me", "PC", 50).unwrap();
        s.capture("a", ContentKind::Text, "me", "PC", 50).unwrap();
        let texts: Vec<_> = s.search("", 100).unwrap().into_iter().map(|i| i.text).collect();
        assert_eq!(texts, ["a", "b"]);
        assert_eq!(s.summary().unwrap().values().filter(|d| **d).count(), 1, "ancien « a » devenu marqueur");
        for n in 0..60 {
            s.capture(&format!("x{n}"), ContentKind::Text, "me", "PC", 50).unwrap();
        }
        assert_eq!(s.search("", 1000).unwrap().len(), 50);
        assert_eq!(s.latest().unwrap().text, "x59");
    }

    #[test]
    fn deletion_wins_and_is_relayed() {
        let mut s = Store::open_in_memory().unwrap();
        let id = Uuid::now_v7();
        assert_eq!(s.apply_remote(id, 1, Some(&payload("secret")), false).unwrap(), Applied::New);
        assert_eq!(s.apply_remote(id, 1, Some(&payload("secret")), false).unwrap(), Applied::Ignored);
        assert_eq!(s.apply_remote(id, 1, None, false).unwrap(), Applied::Deleted);
        assert_eq!(s.apply_remote(id, 1, Some(&payload("secret")), false).unwrap(), Applied::Ignored, "pas de résurrection");
        assert!(s.search("secret", 10).unwrap().is_empty());

        let unknown = Uuid::now_v7();
        s.apply_remote(unknown, 1, None, false).unwrap();
        assert_eq!(s.summary().unwrap().get(&unknown), Some(&true));
    }

    #[test]
    fn missing_for_peer() {
        let mut s = Store::open_in_memory().unwrap();
        let ids = s.capture("a", ContentKind::Text, "me", "PC", 100).unwrap();
        let a = ids[0];
        let b = s.capture("b", ContentKind::Text, "me", "PC", 100).unwrap()[0];
        s.mark_deleted(b).unwrap();
        // Le pair a « a » et « b » vivants : seul le marqueur de « b » lui manque.
        let peer = HashMap::from([(a, false), (b, false)]);
        let missing = s.missing_for(&peer).unwrap();
        assert_eq!(missing.len(), 1);
        assert!(missing[0].deleted && missing[0].item.id == b);
        // Le pair n'a rien : tout lui manque.
        assert_eq!(s.missing_for(&HashMap::new()).unwrap().len(), 2);
    }

    #[test]
    fn image_shows_thumbnail_and_is_not_searchable() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(800, 100).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let png = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png.into_inner());
        let mut s = Store::open_in_memory().unwrap();
        let id = s.capture(&png, ContentKind::Image, "me", "PC", 100).unwrap()[0];
        s.capture("texte", ContentKind::Text, "me", "PC", 100).unwrap();
        let all = s.search("", 10).unwrap();
        assert_eq!(all[1].kind, "image");
        assert!(!all[1].text.is_empty());
        assert_eq!(s.item(id).unwrap().text, png, "contenu complet pour le collage");
        assert!(s.search("A", 10).unwrap().is_empty(), "le base64 ne doit pas être cherché");
    }

    #[test]
    fn search_is_literal_and_case_insensitive() {
        let mut s = Store::open_in_memory().unwrap();
        s.capture("100% Sûr", ContentKind::Text, "me", "PC", 100).unwrap();
        s.capture("1000 sur", ContentKind::Text, "me", "PC", 100).unwrap();
        assert_eq!(s.search("0%", 10).unwrap().len(), 1);
        assert_eq!(s.search("sÛr", 10).unwrap().len(), 2);
        assert_eq!(s.search("SUR", 10).unwrap().len(), 2);
        assert_eq!(s.search("100", 10).unwrap().len(), 2);
        assert_eq!(s.search("% sur", 10).unwrap().len(), 1);
    }
}
