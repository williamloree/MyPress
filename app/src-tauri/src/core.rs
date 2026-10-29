//! Moteur de l'application : historique, groupe d'appareils, fusion des éléments.
//! Ne dépend pas de Tauri, ce qui permet de tester plusieurs appareils dans un même processus.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch, Notify};
use uuid::Uuid;

use mypress_proto::{
    api::*,
    crypto::{self, DeviceKey, GroupKeys},
};

use crate::store::{hash_text, now_ms, Applied, Item, Row, Store};

/// Liens avec l'interface et le système.
pub trait Host: Send + Sync {
    fn history_changed(&self);
    fn status_changed(&self);
    fn write_clipboard(&self, text: &str) -> anyhow::Result<()>;
    /// Image PNG en base64.
    fn write_image(&self, png_b64: &str) -> anyhow::Result<()>;
}

/// Stockage des secrets de l'appareil (coffre du système en production).
pub trait Secrets: Send + Sync {
    fn load(&self) -> Option<String>;
    fn save(&self, value: &str) -> anyhow::Result<()>;
}

#[derive(Serialize, Deserialize)]
struct DeviceSecrets {
    iroh: String,
    x25519: String,
    /// Chiffre l'état du groupe (qui contient la clé du groupe) dans la base locale.
    local: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum MailboxStatus {
    Disabled,
    Connecting,
    Online,
    Error(String),
}

/// Renouvellement de clé à déposer dans l'ancienne boîte aux lettres.
#[derive(Clone, Serialize, Deserialize)]
pub struct PendingRekey {
    pub url: String,
    pub mailbox_id: String,
    pub secret: String,
    pub body: Rekey,
}

pub struct Peer {
    pub member: String,
    pub tx: mpsc::UnboundedSender<PeerMsg>,
    pub conn: iroh::endpoint::Connection,
}

pub struct Core {
    host: Arc<dyn Host>,
    store: Mutex<Store>,
    group: Mutex<GroupState>,
    local_key: [u8; 32],
    pub iroh_key: iroh::SecretKey,
    pub x25519: DeviceKey,
    pub device_id: String,
    pub device_name: String,
    capture_paused: AtomicBool,
    sync_paused: AtomicBool,
    /// Hachage du dernier contenu écrit par l'application, pour ne pas le recapturer.
    self_written: Mutex<Option<String>>,
    /// Fenêtre active avant l'ouverture du panneau.
    pub prev_window: AtomicIsize,
    pub endpoint: OnceLock<iroh::Endpoint>,
    /// Connexions P2P ouvertes, par identifiant de connexion.
    pub peers: Mutex<HashMap<usize, Peer>>,
    /// Codes d'invitation valides : secret → expiration.
    pub invites: Mutex<HashMap<String, i64>>,
    pub mailbox_status: Mutex<MailboxStatus>,
    /// Message à afficher (ex. appareil retiré du groupe).
    pub notice: Mutex<Option<String>>,
    /// Prévient les boucles réseau d'un changement du groupe.
    pub group_rev: watch::Sender<u64>,
    pub kick_mailbox: Notify,
}

/// Ordre total entre deux états : rang puis contenu, pour départager deux modifications simultanées.
fn newer(a: &GroupState, b: &GroupState) -> bool {
    match a.rank().cmp(&b.rank()) {
        std::cmp::Ordering::Equal => serde_json::to_string(a).unwrap() > serde_json::to_string(b).unwrap(),
        o => o.is_gt(),
    }
}

impl Core {
    pub fn new(host: Arc<dyn Host>, secrets: &dyn Secrets, store: Store, device_name: String) -> anyhow::Result<Self> {
        let secrets = match secrets.load().and_then(|s| serde_json::from_str::<DeviceSecrets>(&s).ok()) {
            Some(s) => s,
            None => {
                let s = DeviceSecrets {
                    iroh: crypto::b64(&iroh::SecretKey::generate().to_bytes()),
                    x25519: crypto::b64(&DeviceKey::generate().to_bytes()),
                    local: crypto::b64(&crypto::random_key()),
                };
                secrets.save(&serde_json::to_string(&s)?)?;
                s
            }
        };
        let iroh_key = iroh::SecretKey::from_bytes(&crypto::key_from_b64(&secrets.iroh)?);
        let x25519 = DeviceKey::from_bytes(crypto::key_from_b64(&secrets.x25519)?);
        let local_key = crypto::key_from_b64(&secrets.local)?;
        let device_id = iroh_key.public().to_string();

        let me = Member { id: device_id.clone(), name: device_name.clone(), x25519: x25519.public_b64(), added_at: now_ms() };
        let group = store
            .get("group")
            .and_then(|blob| crypto::decrypt_state(&local_key, &blob).ok())
            .filter(|g| g.is_member(&device_id))
            .unwrap_or_else(|| solo_group(me, DEFAULT_HISTORY_LIMIT));
        store.set("group", &crypto::encrypt_state(&local_key, &group));

        Ok(Self {
            capture_paused: AtomicBool::new(store.get("capture_paused").as_deref() == Some("1")),
            sync_paused: AtomicBool::new(store.get("sync_paused").as_deref() == Some("1")),
            host,
            store: Mutex::new(store),
            group: Mutex::new(group),
            local_key,
            iroh_key,
            x25519,
            device_id,
            device_name,
            self_written: Mutex::new(None),
            prev_window: AtomicIsize::new(0),
            endpoint: OnceLock::new(),
            peers: Mutex::new(HashMap::new()),
            invites: Mutex::new(HashMap::new()),
            mailbox_status: Mutex::new(MailboxStatus::Disabled),
            notice: Mutex::new(None),
            group_rev: watch::channel(0).0,
            kick_mailbox: Notify::new(),
        })
    }

    pub fn with_store<T>(&self, f: impl FnOnce(&mut Store) -> T) -> T {
        f(&mut self.store.lock().unwrap())
    }

    pub fn me(&self) -> Member {
        Member { id: self.device_id.clone(), name: self.device_name.clone(), x25519: self.x25519.public_b64(), added_at: now_ms() }
    }

    // ---------- état ----------

    pub fn capture_paused(&self) -> bool {
        self.capture_paused.load(Ordering::Relaxed)
    }

    pub fn sync_paused(&self) -> bool {
        self.sync_paused.load(Ordering::Relaxed)
    }

    pub fn set_capture_paused(&self, v: bool) {
        self.capture_paused.store(v, Ordering::Relaxed);
        self.with_store(|s| s.set("capture_paused", if v { "1" } else { "0" }));
        self.host.status_changed();
    }

    /// La pause coupe les connexions P2P et la boîte aux lettres.
    pub fn set_sync_paused(&self, v: bool) {
        self.sync_paused.store(v, Ordering::Relaxed);
        self.with_store(|s| s.set("sync_paused", if v { "1" } else { "0" }));
        if v {
            for (_, p) in self.peers.lock().unwrap().drain() {
                p.conn.close(0u32.into(), b"pause");
            }
        }
        self.group_rev.send_modify(|r| *r += 1);
        self.host.status_changed();
    }

    pub fn set_mailbox_status(&self, s: MailboxStatus) {
        let mut cur = self.mailbox_status.lock().unwrap();
        if *cur != s {
            *cur = s;
            drop(cur);
            self.host.status_changed();
        }
    }

    pub fn status_changed(&self) {
        self.host.status_changed();
    }

    /// Membres actuellement joignables en P2P.
    pub fn online_members(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.peers.lock().unwrap().values().map(|p| p.member.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    // ---------- groupe ----------

    pub fn group(&self) -> GroupState {
        self.group.lock().unwrap().clone()
    }

    pub fn keys(&self) -> GroupKeys {
        GroupKeys::from_state(&self.group()).expect("clé de groupe valide")
    }

    pub fn history_limit(&self) -> u32 {
        self.group.lock().unwrap().history_limit
    }

    /// Remplace l'état du groupe et en tire les conséquences locales.
    fn set_group(&self, new: GroupState, notify_removed: bool) {
        let old = std::mem::replace(&mut *self.group.lock().unwrap(), new.clone());
        self.with_store(|s| {
            s.set("group", &crypto::encrypt_state(&self.local_key, &new));
            // Nouvelle clé ou nouvelle boîte : tout est à renvoyer.
            if old.epoch != new.epoch || old.key != new.key || old.mailbox != new.mailbox {
                let _ = s.mark_all_unsynced();
            }
            let _ = s.prune(new.history_limit);
        });
        // Les appareils retirés sont déconnectés ; celui qui révoque les prévient (sans la nouvelle clé).
        self.peers.lock().unwrap().retain(|_, p| {
            let keep = new.is_member(&p.member);
            if !keep {
                if notify_removed {
                    let _ = p.tx.send(PeerMsg::Denied { reason: "revoked".into() });
                }
                let conn = p.conn.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    conn.close(1u32.into(), b"revoque");
                });
            }
            keep
        });
        self.group_rev.send_modify(|r| *r += 1);
        self.host.history_changed();
        self.host.status_changed();
    }

    /// Adopte un état reçu s'il est plus récent. Retourne `true` s'il a été adopté.
    pub fn adopt(&self, new: GroupState) -> bool {
        if crypto::key_from_b64(&new.key).is_err() || !newer(&new, &self.group()) {
            return false;
        }
        if !new.is_member(&self.device_id) {
            self.become_solo("Cet appareil a été retiré du groupe.");
            return true;
        }
        self.set_group(new.clone(), false);
        self.broadcast(PeerMsg::Group { state: new });
        self.broadcast(self.hello());
        self.kick_mailbox.notify_one();
        true
    }

    /// Rejoindre un groupe : l'état reçu remplace l'actuel quel que soit son rang.
    pub fn join(&self, state: GroupState) {
        *self.notice.lock().unwrap() = None;
        self.set_group(state, false);
        self.kick_mailbox.notify_one();
    }

    /// Modification locale de l'état (paramètres, membres).
    pub fn update_group(&self, f: impl FnOnce(&mut GroupState)) -> GroupState {
        let mut state = self.group();
        f(&mut state);
        state.version += 1;
        self.set_group(state.clone(), false);
        self.broadcast(PeerMsg::Group { state: state.clone() });
        self.kick_mailbox.notify_one();
        state
    }

    pub fn become_solo(&self, notice: &str) {
        let limit = self.history_limit();
        *self.notice.lock().unwrap() = Some(notice.to_string());
        self.set_group(solo_group(self.me(), limit), false);
    }

    /// Retire un appareil : nouvelle clé de groupe, transmise aux appareils restants
    /// en P2P et, via l'ancienne boîte aux lettres, chiffrée pour chacun d'eux.
    pub fn revoke(&self, member_id: &str) -> anyhow::Result<()> {
        let old = self.group();
        if member_id == self.device_id || !old.is_member(member_id) {
            anyhow::bail!("appareil introuvable");
        }
        let new = GroupState {
            epoch: old.epoch + 1,
            version: 0,
            key: crypto::b64(&crypto::random_key()),
            members: old.members.iter().filter(|m| m.id != member_id).cloned().collect(),
            history_limit: old.history_limit,
            mailbox: old.mailbox.clone(),
        };
        if let Some(url) = &old.mailbox {
            let keys = GroupKeys::from_state(&old)?;
            let wrapped = new
                .members
                .iter()
                .filter(|m| m.id != self.device_id)
                .map(|m| Ok(WrappedKey { recipient: m.id.clone(), blob: crypto::wrap_state(&m.x25519, &new)? }))
                .collect::<Result<Vec<_>, crypto::CryptoError>>()?;
            let pending = PendingRekey { url: url.clone(), mailbox_id: keys.mailbox_id, secret: keys.mailbox_secret, body: Rekey { wrapped } };
            self.with_store(|s| s.set("pending_rekey", &serde_json::to_string(&pending).unwrap()));
        }
        self.set_group(new.clone(), true);
        self.broadcast(PeerMsg::Group { state: new });
        self.kick_mailbox.notify_one();
        Ok(())
    }

    /// Quitte le groupe : prévient les autres puis repart avec un groupe à soi.
    pub async fn leave(&self) {
        let mut state = self.group();
        state.members.retain(|m| m.id != self.device_id);
        state.version += 1;
        self.broadcast(PeerMsg::Group { state });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        self.become_solo("Vous avez quitté le groupe.");
        *self.notice.lock().unwrap() = None;
    }

    // ---------- éléments ----------

    pub fn record_of(&self, row: &Row, keys: &GroupKeys) -> Record {
        let blob = (!row.deleted).then(|| {
            let payload = ItemPayload {
                kind: ContentKind::parse(row.item.kind),
                text: row.item.text.clone(),
                device_name: row.item.device_name.clone(),
                device_id: row.item.device_id.clone(),
            };
            crypto::encrypt_item(&keys.enc_key, row.item.id, &payload)
        });
        Record { id: row.item.id, created_at: row.item.created_at, blob }
    }

    /// Diffuse des lignes modifiées localement aux appareils connectés et à la boîte aux lettres.
    fn propagate(&self, ids: &[uuid::Uuid]) {
        let keys = self.keys();
        let records: Vec<Record> = self
            .with_store(|s| ids.iter().filter_map(|id| s.row(*id)).collect::<Vec<_>>())
            .iter()
            .map(|r| self.record_of(r, &keys))
            .collect();
        if !records.is_empty() {
            self.broadcast(PeerMsg::Records { records });
        }
        self.kick_mailbox.notify_one();
        self.host.history_changed();
    }

    /// Appelé par la surveillance du presse-papiers.
    pub fn on_clipboard_text(&self, text: String) {
        self.on_clipboard(ContentKind::detect(&text), text);
    }

    /// Image PNG en base64.
    pub fn on_clipboard_image(&self, png_b64: String) {
        self.on_clipboard(ContentKind::Image, png_b64);
    }

    fn on_clipboard(&self, kind: ContentKind, text: String) {
        if self.capture_paused() {
            return;
        }
        {
            // Notre propre écriture : ignorée une seule fois, pour éviter les boucles entre appareils.
            let mut own = self.self_written.lock().unwrap();
            if own.as_deref() == Some(hash_text(&text).as_str()) {
                *own = None;
                return;
            }
        }
        let limit = self.history_limit();
        match self.with_store(|s| s.capture(&text, kind, &self.device_id, &self.device_name, limit)) {
            Ok(ids) => self.propagate(&ids),
            Err(e) => tracing::error!("enregistrement impossible : {e}"),
        }
    }

    pub fn delete(&self, id: Uuid) -> anyhow::Result<()> {
        if self.with_store(|s| s.mark_deleted(id))? {
            self.propagate(&[id]);
        }
        Ok(())
    }

    pub fn clear(&self) -> anyhow::Result<()> {
        let ids = self.with_store(|s| s.clear())?;
        self.propagate(&ids);
        Ok(())
    }

    pub fn write_clipboard(&self, text: &str) {
        self.write(text, false);
    }

    pub fn write_item(&self, item: &Item) {
        self.write(&item.text, item.kind == ContentKind::Image.as_str());
    }

    fn write(&self, text: &str, image: bool) {
        *self.self_written.lock().unwrap() = Some(hash_text(text));
        let res = if image { self.host.write_image(text) } else { self.host.write_clipboard(text) };
        if let Err(e) = res {
            *self.self_written.lock().unwrap() = None;
            tracing::warn!("écriture du presse-papiers impossible : {e}");
        }
    }

    /// Applique des éléments reçus d'un autre appareil ou de la boîte aux lettres.
    pub fn apply_records(&self, records: &[Record], from_mailbox: bool) {
        let keys = self.keys();
        let limit = self.history_limit();
        let mut new_ids = Vec::new();
        let mut changed = false;
        self.with_store(|s| {
            for r in records {
                let payload = match &r.blob {
                    Some(blob) => match crypto::decrypt_item(&keys.enc_key, r.id, blob) {
                        Ok(p) => Some(p),
                        Err(_) => {
                            tracing::warn!("élément {} illisible (autre clé ?)", r.id);
                            continue;
                        }
                    },
                    None => None,
                };
                match s.apply_remote(r.id, r.created_at, payload.as_ref(), from_mailbox) {
                    Ok(Applied::New) => {
                        new_ids.push(r.id);
                        changed = true;
                    }
                    Ok(Applied::Deleted) => changed = true,
                    Ok(Applied::Ignored) => {}
                    Err(e) => tracing::error!("enregistrement impossible : {e}"),
                }
            }
            let _ = s.prune(limit);
        });
        if !changed {
            return;
        }
        // Le dernier élément vient d'arriver d'un autre appareil : Ctrl+V doit le coller.
        if let Some(latest) = self.with_store(|s| s.latest()) {
            if new_ids.contains(&latest.id) && latest.device_id != self.device_id {
                self.write_item(&latest);
            }
        }
        if !from_mailbox {
            self.kick_mailbox.notify_one();
        }
        self.host.history_changed();
    }

    // ---------- P2P ----------

    pub fn hello(&self) -> PeerMsg {
        let g = self.group();
        let ids = self.with_store(|s| s.summary()).unwrap_or_default().into_iter().collect();
        PeerMsg::Hello { epoch: g.epoch, version: g.version, ids }
    }

    /// Éléments qui manquent à un appareil d'après son résumé.
    pub fn missing_for(&self, ids: &[(Uuid, bool)]) -> Vec<Record> {
        let peer: HashMap<Uuid, bool> = ids.iter().copied().collect();
        let keys = self.keys();
        let rows = self.with_store(|s| s.missing_for(&peer)).unwrap_or_default();
        rows.iter().map(|r| self.record_of(r, &keys)).collect()
    }

    pub fn broadcast(&self, msg: PeerMsg) {
        for p in self.peers.lock().unwrap().values() {
            let _ = p.tx.send(msg.clone());
        }
    }

    /// Crée un code d'invitation à usage unique, valable 15 minutes.
    pub fn create_invite(&self) -> String {
        let token = crypto::random_key()[..16].to_vec();
        let token_b64 = crypto::b64(&token);
        let mut invites = self.invites.lock().unwrap();
        invites.retain(|_, exp| *exp > now_ms());
        invites.insert(token_b64, now_ms() + 15 * 60 * 1000);
        self.invite_code("MP1-", &token)
    }

    /// Code d'invitation : identifiant de cet appareil suivi du secret.
    pub fn invite_code(&self, prefix: &str, token: &[u8]) -> String {
        let mut raw = self.iroh_key.public().as_bytes().to_vec();
        raw.extend_from_slice(token);
        format!("{prefix}{}", base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw))
    }

    pub fn consume_invite(&self, token: &str) -> bool {
        matches!(self.invites.lock().unwrap().remove(token), Some(exp) if exp > now_ms())
    }
}

/// Décode un code d'invitation : (appareil invitant, secret).
pub fn parse_invite(code: &str) -> anyhow::Result<(iroh::EndpointId, String)> {
    parse_code(code, "MP1-")
}

pub fn parse_code(code: &str, prefix: &str) -> anyhow::Result<(iroh::EndpointId, String)> {
    let raw = code.trim().strip_prefix(prefix).ok_or_else(|| anyhow::anyhow!("code d'invitation invalide"))?;
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw)
        .map_err(|_| anyhow::anyhow!("code d'invitation invalide"))?;
    if bytes.len() != 48 {
        anyhow::bail!("code d'invitation invalide");
    }
    let id = iroh::EndpointId::from_bytes(bytes[..32].try_into().unwrap())
        .map_err(|_| anyhow::anyhow!("code d'invitation invalide"))?;
    Ok((id, crypto::b64(&bytes[32..])))
}

fn solo_group(me: Member, history_limit: u32) -> GroupState {
    GroupState {
        epoch: 0,
        version: 0,
        key: crypto::b64(&crypto::random_key()),
        members: vec![me],
        history_limit,
        mailbox: None,
    }
}
