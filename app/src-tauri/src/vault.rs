//! Coffre de mots de passe, partagé par les appareils du groupe.
//!
//! Les entrées sont chiffrées avec les clés du coffre ([`VaultKeys`], dans l'état du
//! groupe) et circulent telles quelles : disque, P2P et boîte aux lettres ne voient que
//! des révisions chiffrées. Modifier une entrée crée une révision et supprime l'ancienne ;
//! pour une même entrée, la révision la plus récente l'emporte.
//!
//! Avec une phrase de passe, les clés sont scellées (Argon2id) : il faut déverrouiller
//! le coffre pour lire ou écrire, et il se reverrouille après un délai d'inactivité.
//! La synchronisation, elle, continue sans déverrouillage.
//!
//! Retirer un appareil ajoute une nouvelle clé et rechiffre les entrées : l'appareil
//! retiré ne lit plus les modifications suivantes (ce qu'il a déjà lu reste lu).

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI64, Ordering},
        Mutex,
    },
};

use serde::Serialize;
use uuid::Uuid;

use mypress_proto::{
    api::{GroupState, PeerMsg, Record, VaultFields, VaultKeys, VaultPayload},
    crypto,
};

use crate::{
    core::Core,
    store::{fold, now_ms, Applied},
};

/// Délais de verrouillage automatique proposés (minutes, 0 = jamais).
pub const AUTO_LOCK_CHOICES: [u32; 5] = [1, 5, 15, 60, 0];
const DEFAULT_AUTO_LOCK: u32 = 15;
pub const MIN_PASSPHRASE: usize = 10;
const MAX_ENTRY_BYTES: usize = 64 * 1024;

type Key = [u8; 32];

/// Coffre déverrouillé (avec phrase de passe uniquement).
struct Unlocked {
    keys: Vec<Key>,
    /// Clé dérivée de la phrase de passe et son sel : permet de resceller les clés
    /// (nouvelle clé après un retrait) sans redemander la phrase.
    derived: Key,
    salt: String,
}

#[derive(Default)]
pub struct Session {
    unlocked: Mutex<Option<Unlocked>>,
    last_used: AtomicI64,
}

#[derive(Serialize)]
pub struct VaultStatus {
    /// "none" (pas encore de coffre), "open" (sans phrase de passe), "sealed".
    mode: &'static str,
    unlocked: bool,
    auto_lock: u32,
    auto_lock_choices: Vec<u32>,
    min_passphrase: usize,
}

/// Entrée sans son mot de passe, pour la liste.
#[derive(Serialize)]
pub struct EntrySummary {
    id: Uuid,
    name: String,
    url: String,
    username: String,
    has_password: bool,
    updated_at: i64,
}

#[derive(Serialize)]
pub struct Entry {
    id: Uuid,
    updated_at: i64,
    device_name: String,
    fields: VaultFields,
}

/// Révision déchiffrée.
struct Revision {
    id: Uuid,
    payload: VaultPayload,
}

fn decode(keys: &[String]) -> Vec<Key> {
    keys.iter().filter_map(|k| crypto::key_from_b64(k).ok()).collect()
}

fn encode(keys: &[Key]) -> Vec<String> {
    keys.iter().map(|k| crypto::b64(k)).collect()
}

/// Révision gagnante de chaque entrée, et révisions perdantes (modifications simultanées).
fn winners(revs: Vec<Revision>) -> (HashMap<Uuid, Revision>, Vec<Uuid>) {
    let mut best: HashMap<Uuid, Revision> = HashMap::new();
    let mut losers = Vec::new();
    for r in revs {
        match best.get(&r.payload.entry) {
            Some(b) if (b.payload.updated_at, b.id) >= (r.payload.updated_at, r.id) => losers.push(r.id),
            _ => {
                if let Some(old) = best.insert(r.payload.entry, r) {
                    losers.push(old.id);
                }
            }
        }
    }
    (best, losers)
}

impl Core {
    // ---------- clés ----------

    /// Clés du groupe utilisables maintenant ; `None` si le coffre est verrouillé.
    fn vault_group_keys(&self) -> Option<Vec<Key>> {
        match self.group().vault {
            None => Some(Vec::new()),
            Some(VaultKeys::Open { keys }) => Some(decode(&keys)),
            Some(VaultKeys::Sealed { .. }) => self.vault.unlocked.lock().unwrap().as_ref().map(|u| u.keys.clone()),
        }
    }

    fn vault_read_keys(&self) -> anyhow::Result<Vec<Key>> {
        let mut keys = self.vault_extra_keys();
        keys.extend(self.vault_group_keys().ok_or_else(|| anyhow::anyhow!("coffre verrouillé"))?);
        self.vault.last_used.store(now_ms(), Ordering::Relaxed);
        Ok(keys)
    }

    /// Clé d'écriture ; crée le coffre à la première entrée.
    fn vault_write_key(&self) -> anyhow::Result<Key> {
        if self.group().vault.is_none() {
            self.update_group(|g| {
                if g.vault.is_none() {
                    g.vault = Some(VaultKeys::Open { keys: vec![crypto::b64(&crypto::random_key())] });
                }
            });
        }
        self.vault_read_keys()?;
        let keys = self.vault_group_keys().unwrap_or_default();
        keys.last().copied().ok_or_else(|| anyhow::anyhow!("clé du coffre illisible"))
    }

    /// Clés apportées par cet appareil en rejoignant un groupe dont le coffre était
    /// verrouillé : ajoutées au coffre du groupe au prochain déverrouillage.
    fn vault_extra_keys(&self) -> Vec<Key> {
        self.with_store(|s| s.get("vault_extra_keys"))
            .and_then(|blob| crypto::open(&self.local_key, b"mypress vault-extra v1", &blob).ok())
            .and_then(|plain| serde_json::from_slice::<Vec<String>>(&plain).ok())
            .map(|keys| decode(&keys))
            .unwrap_or_default()
    }

    fn set_vault_extra_keys(&self, keys: &[Key]) {
        if keys.is_empty() {
            self.with_store(|s| s.delete_kv("vault_extra_keys"));
        } else {
            let blob = crypto::seal(&self.local_key, b"mypress vault-extra v1", &serde_json::to_vec(&encode(keys)).unwrap());
            self.with_store(|s| s.set("vault_extra_keys", &blob));
        }
    }

    /// Avant de rejoindre un autre groupe : clés nécessaires pour relire les entrées de cet appareil.
    pub fn vault_keys_for_join(&self) -> anyhow::Result<Vec<Key>> {
        let mut keys = self.vault_extra_keys();
        match self.vault_group_keys() {
            Some(k) => keys.extend(k),
            None if !self.with_store(|s| s.vault_live())?.is_empty() => {
                anyhow::bail!("déverrouillez le coffre avant de rejoindre un autre ordinateur, sinon ses entrées seraient perdues")
            }
            None => {}
        }
        Ok(keys)
    }

    /// Ajoute au coffre du groupe des clés qu'il n'a pas (entrées apportées par un appareil).
    /// Les plus récentes du groupe restent en dernier : ce sont elles qui chiffrent.
    pub fn vault_merge_keys(&self, keys: Vec<Key>) {
        let known = self.vault_group_keys();
        let mut missing: Vec<Key> = Vec::new();
        for k in keys {
            if !known.as_ref().is_some_and(|known| known.contains(&k)) && !missing.contains(&k) {
                missing.push(k);
            }
        }
        if missing.is_empty() {
            self.set_vault_extra_keys(&[]);
            return;
        }
        let derived = self.vault.unlocked.lock().unwrap().as_ref().map(|u| u.derived);
        let merged = match (self.group().vault, known, derived) {
            (None, ..) => VaultKeys::Open { keys: encode(&missing) },
            (Some(VaultKeys::Open { keys }), ..) => VaultKeys::Open { keys: [encode(&missing), keys].concat() },
            (Some(VaultKeys::Sealed { salt, .. }), Some(known), Some(derived)) => {
                VaultKeys::Sealed { salt, blob: crypto::seal_vault_keys(&derived, &[missing, known].concat()) }
            }
            // Verrouillé : à fusionner au prochain déverrouillage.
            (Some(VaultKeys::Sealed { .. }), ..) => {
                self.set_vault_extra_keys(&missing);
                return;
            }
        };
        self.update_group(|g| g.vault = Some(merged));
        self.set_vault_extra_keys(&[]);
    }

    /// Nouvelles clés pour un retrait d'appareil : une clé de plus, qui chiffrera désormais.
    /// Coffre verrouillé : le renouvellement attend le prochain déverrouillage.
    pub(crate) fn vault_rotated(&self, vault: &Option<VaultKeys>) -> Option<VaultKeys> {
        let fresh = crypto::random_key();
        match vault {
            None => None,
            Some(VaultKeys::Open { keys }) => {
                let mut keys = keys.clone();
                keys.push(crypto::b64(&fresh));
                Some(VaultKeys::Open { keys })
            }
            Some(VaultKeys::Sealed { salt, .. }) => {
                let guard = self.vault.unlocked.lock().unwrap();
                match guard.as_ref().filter(|u| &u.salt == salt) {
                    Some(u) => {
                        let mut keys = u.keys.clone();
                        keys.push(fresh);
                        Some(VaultKeys::Sealed { salt: salt.clone(), blob: crypto::seal_vault_keys(&u.derived, &keys) })
                    }
                    None => {
                        self.with_store(|s| s.set("vault_rotate_pending", "1"));
                        vault.clone()
                    }
                }
            }
        }
    }

    /// Rechiffre toutes les entrées avec la clé la plus récente.
    pub(crate) fn vault_reencrypt(&self) {
        let (Ok(revs), Some(key)) = (self.vault_revisions(), self.vault_group_keys().and_then(|k| k.last().copied())) else {
            return;
        };
        let (best, losers) = winners(revs);
        let mut changed = Vec::new();
        let res = self.with_store(|s| -> rusqlite::Result<()> {
            for (_, r) in best {
                let id = Uuid::now_v7();
                s.vault_insert(id, &crypto::encrypt_vault(&key, id, &r.payload))?;
                s.vault_delete(r.id)?;
                changed.extend([id, r.id]);
            }
            for id in losers {
                if s.vault_delete(id)? {
                    changed.push(id);
                }
            }
            Ok(())
        });
        if let Err(e) = res {
            tracing::error!("rechiffrement du coffre impossible : {e}");
        }
        self.vault_propagate(&changed);
    }

    /// Les clés du groupe ont changé (autre appareil, retrait, phrase de passe) :
    /// la session reste ouverte si la clé dérivée ouvre encore les nouvelles clés.
    pub(crate) fn vault_on_group_change(&self, old: &GroupState, new: &GroupState) {
        if old.vault == new.vault {
            return;
        }
        {
            let mut guard = self.vault.unlocked.lock().unwrap();
            match &new.vault {
                Some(VaultKeys::Sealed { salt, blob }) => {
                    let keys = guard
                        .as_ref()
                        .filter(|u| &u.salt == salt)
                        .and_then(|u| crypto::open_vault_keys(&u.derived, blob).ok());
                    match keys {
                        Some(keys) => guard.as_mut().unwrap().keys = keys,
                        None => *guard = None,
                    }
                }
                _ => *guard = None,
            }
        }
        self.host.vault_changed();
    }

    // ---------- verrouillage ----------

    fn auto_lock_minutes(&self) -> u32 {
        self.with_store(|s| s.get("vault_auto_lock")).and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_AUTO_LOCK)
    }

    pub fn vault_status(&self) -> VaultStatus {
        let (mode, unlocked) = match self.group().vault {
            None => ("none", true),
            Some(VaultKeys::Open { .. }) => ("open", true),
            Some(VaultKeys::Sealed { .. }) => ("sealed", self.vault.unlocked.lock().unwrap().is_some()),
        };
        VaultStatus {
            mode,
            unlocked,
            auto_lock: self.auto_lock_minutes(),
            auto_lock_choices: AUTO_LOCK_CHOICES.to_vec(),
            min_passphrase: MIN_PASSPHRASE,
        }
    }

    /// Lent (Argon2id) : à appeler hors du fil de l'interface.
    pub fn vault_unlock(&self, passphrase: &str) -> anyhow::Result<()> {
        let Some(VaultKeys::Sealed { salt, blob }) = self.group().vault else { return Ok(()) };
        let derived = crypto::passphrase_key(passphrase, &crypto::salt_from_b64(&salt)?)?;
        let keys = crypto::open_vault_keys(&derived, &blob).map_err(|_| anyhow::anyhow!("phrase de passe incorrecte"))?;
        *self.vault.unlocked.lock().unwrap() = Some(Unlocked { keys, derived, salt });
        self.vault.last_used.store(now_ms(), Ordering::Relaxed);
        self.vault_merge_keys(self.vault_extra_keys());
        if self.with_store(|s| s.get("vault_rotate_pending")).is_some() {
            self.with_store(|s| s.delete_kv("vault_rotate_pending"));
            let rotated = self.vault_rotated(&self.group().vault);
            self.update_group(|g| g.vault = rotated);
            self.vault_reencrypt();
        }
        self.host.vault_changed();
        Ok(())
    }

    pub fn vault_lock(&self) {
        if self.vault.unlocked.lock().unwrap().take().is_some() {
            self.host.vault_changed();
        }
    }

    /// Appelé régulièrement : verrouille après le délai d'inactivité.
    pub fn vault_auto_lock(&self) {
        let minutes = self.auto_lock_minutes() as i64;
        let idle = now_ms() - self.vault.last_used.load(Ordering::Relaxed);
        if minutes > 0 && idle > minutes * 60_000 {
            self.vault_lock();
        }
    }

    pub fn vault_set_auto_lock(&self, minutes: u32) -> anyhow::Result<()> {
        if !AUTO_LOCK_CHOICES.contains(&minutes) {
            anyhow::bail!("valeur non prise en charge");
        }
        self.with_store(|s| s.set("vault_auto_lock", &minutes.to_string()));
        self.host.vault_changed();
        Ok(())
    }

    /// Définit, change (`Some`) ou retire (`None`) la phrase de passe, pour tous les
    /// appareils du groupe. Coffre déverrouillé requis. Lent (Argon2id).
    pub fn vault_set_passphrase(&self, passphrase: Option<&str>) -> anyhow::Result<()> {
        let mut keys = self.vault_read_keys()?;
        keys.dedup();
        if keys.is_empty() {
            keys.push(crypto::random_key());
        }
        let vault = match passphrase {
            Some(p) => {
                if p.chars().count() < MIN_PASSPHRASE {
                    anyhow::bail!("phrase de passe trop courte ({MIN_PASSPHRASE} caractères minimum)");
                }
                let salt = crypto::random_salt();
                let derived = crypto::passphrase_key(p, &crypto::salt_from_b64(&salt)?)?;
                let blob = crypto::seal_vault_keys(&derived, &keys);
                *self.vault.unlocked.lock().unwrap() = Some(Unlocked { keys, derived, salt: salt.clone() });
                VaultKeys::Sealed { salt, blob }
            }
            None => VaultKeys::Open { keys: encode(&keys) },
        };
        self.update_group(|g| g.vault = Some(vault));
        self.set_vault_extra_keys(&[]);
        self.vault.last_used.store(now_ms(), Ordering::Relaxed);
        self.host.vault_changed();
        Ok(())
    }

    // ---------- entrées ----------

    fn vault_revisions(&self) -> anyhow::Result<Vec<Revision>> {
        let keys = self.vault_read_keys()?;
        let records = self.with_store(|s| s.vault_live())?;
        Ok(records
            .into_iter()
            .filter_map(|r| {
                let payload = crypto::decrypt_vault(&keys, r.id, r.blob.as_deref()?);
                if payload.is_err() {
                    tracing::warn!("révision {} du coffre illisible (autre clé ?)", r.id);
                }
                Some(Revision { id: r.id, payload: payload.ok()? })
            })
            .collect())
    }

    /// Entrées triées par nom, filtrées sans tenir compte de la casse ni des accents.
    pub fn vault_list(&self, query: &str) -> anyhow::Result<Vec<EntrySummary>> {
        let needle = fold(query.trim());
        let mut out: Vec<EntrySummary> = winners(self.vault_revisions()?)
            .0
            .into_values()
            .map(|r| r.payload)
            .filter(|p| {
                let f = &p.fields;
                needle.is_empty() || [&f.name, &f.url, &f.username, &f.notes].iter().any(|t| fold(t).contains(&needle))
            })
            .map(|p| EntrySummary {
                id: p.entry,
                has_password: !p.fields.password.is_empty(),
                name: p.fields.name,
                url: p.fields.url,
                username: p.fields.username,
                updated_at: p.updated_at,
            })
            .collect();
        out.sort_by_cached_key(|e| (fold(&e.name), e.id));
        Ok(out)
    }

    pub fn vault_entry(&self, entry: Uuid) -> anyhow::Result<Entry> {
        let mut best = winners(self.vault_revisions()?).0;
        let r = best.remove(&entry).ok_or_else(|| anyhow::anyhow!("entrée introuvable"))?;
        Ok(Entry { id: entry, updated_at: r.payload.updated_at, device_name: r.payload.device_name, fields: r.payload.fields })
    }

    /// Crée (`entry == None`) ou modifie une entrée. Retourne son identifiant.
    pub fn vault_save(&self, entry: Option<Uuid>, mut fields: VaultFields) -> anyhow::Result<Uuid> {
        fields.name = fields.name.trim().to_string();
        fields.url = fields.url.trim().to_string();
        if fields.name.is_empty() {
            anyhow::bail!("donnez un nom à l'entrée");
        }
        let f = &fields;
        if [&f.name, &f.url, &f.username, &f.password, &f.notes].iter().map(|s| s.len()).sum::<usize>() > MAX_ENTRY_BYTES {
            anyhow::bail!("entrée trop volumineuse (64 Ko maximum)");
        }
        let key = self.vault_write_key()?;
        let revs = self.vault_revisions()?;
        let entry_id = entry.unwrap_or_else(Uuid::now_v7);
        let previous: Vec<&Revision> = revs.iter().filter(|r| r.payload.entry == entry_id).collect();
        if entry.is_some() && previous.is_empty() {
            anyhow::bail!("entrée introuvable (supprimée sur un autre appareil ?)");
        }
        // Toujours plus récente que la révision remplacée, même si les horloges divergent.
        let updated_at = previous.iter().map(|r| r.payload.updated_at + 1).fold(now_ms(), i64::max);
        let id = Uuid::now_v7();
        let payload = VaultPayload { entry: entry_id, updated_at, device_name: self.device_name.clone(), fields };
        let mut changed = vec![id];
        self.with_store(|s| -> rusqlite::Result<()> {
            s.vault_insert(id, &crypto::encrypt_vault(&key, id, &payload))?;
            for r in &previous {
                if s.vault_delete(r.id)? {
                    changed.push(r.id);
                }
            }
            Ok(())
        })?;
        self.vault_propagate(&changed);
        Ok(entry_id)
    }

    pub fn vault_delete(&self, entry: Uuid) -> anyhow::Result<()> {
        let revs = self.vault_revisions()?;
        let mut changed = Vec::new();
        self.with_store(|s| -> rusqlite::Result<()> {
            for r in revs.iter().filter(|r| r.payload.entry == entry) {
                if s.vault_delete(r.id)? {
                    changed.push(r.id);
                }
            }
            Ok(())
        })?;
        self.vault_propagate(&changed);
        Ok(())
    }

    /// Copie un champ sans passer par l'historique ; effacé du presse-papiers après 30 s.
    pub fn vault_copy(&self, entry: Uuid, field: &str) -> anyhow::Result<()> {
        let f = self.vault_entry(entry)?.fields;
        let text = match field {
            "username" => f.username,
            "password" => f.password,
            "url" => f.url,
            _ => anyhow::bail!("champ inconnu"),
        };
        if text.is_empty() {
            anyhow::bail!("champ vide");
        }
        self.write_secret(text);
        Ok(())
    }

    // ---------- synchronisation ----------

    fn vault_propagate(&self, ids: &[Uuid]) {
        let records: Vec<Record> = self.with_store(|s| ids.iter().filter_map(|id| s.vault_record(*id)).collect());
        if !records.is_empty() {
            self.broadcast_vault(PeerMsg::VaultRecords { records });
        }
        self.kick_mailbox.notify_one();
        self.host.vault_changed();
    }

    pub fn vault_summary(&self) -> Vec<(Uuid, bool)> {
        self.with_store(|s| s.vault_summary()).unwrap_or_default().into_iter().collect()
    }

    pub fn vault_missing_for(&self, ids: &[(Uuid, bool)]) -> Vec<Record> {
        let peer: HashMap<Uuid, bool> = ids.iter().copied().collect();
        self.with_store(|s| s.vault_missing_for(&peer)).unwrap_or_default()
    }

    /// Révisions reçues d'un autre appareil ou de la boîte aux lettres, stockées
    /// chiffrées : pas besoin que le coffre soit déverrouillé.
    pub fn apply_vault_records(&self, records: &[Record], from_mailbox: bool) {
        let mut changed = false;
        self.with_store(|s| {
            for r in records {
                match s.vault_apply(r, from_mailbox) {
                    Ok(Applied::New | Applied::Deleted) => changed = true,
                    Ok(Applied::Ignored) => {}
                    Err(e) => tracing::error!("révision du coffre non enregistrée : {e}"),
                }
            }
        });
        if changed {
            if !from_mailbox {
                self.kick_mailbox.notify_one();
            }
            self.host.vault_changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(entry: Uuid, updated_at: i64) -> Revision {
        Revision {
            id: Uuid::now_v7(),
            payload: VaultPayload { entry, updated_at, device_name: String::new(), fields: VaultFields::default() },
        }
    }

    #[test]
    fn latest_revision_wins() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let old = rev(a, 10);
        let new = rev(a, 20);
        let same_time_later_id = rev(a, 20);
        let other = rev(b, 1);
        let (new_id, tie_id, old_id) = (new.id, same_time_later_id.id, old.id);
        let (best, mut losers) = winners(vec![new, old, same_time_later_id, other]);
        assert_eq!(best.len(), 2);
        assert_eq!(best[&a].id, tie_id, "égalité : identifiant le plus récent");
        losers.sort();
        let mut expected = vec![new_id, old_id];
        expected.sort();
        assert_eq!(losers, expected);
    }
}
