//! Boîte aux lettres optionnelle : relaie les éléments chiffrés entre appareils
//! qui ne sont pas allumés en même temps. Le P2P fonctionne sans elle.

use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use serde::{de::DeserializeOwned, Serialize};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use uuid::Uuid;

use mypress_proto::{
    api::*,
    crypto::{self, GroupKeys},
};

use crate::core::{Core, MailboxStatus, PendingRekey};

/// Taille cible d'un envoi d'éléments à la boîte aux lettres.
const PUSH_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
enum MbError {
    /// Le groupe a changé de clé : voir le message de renouvellement.
    Gone,
    Other(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for MbError {
    fn from(e: E) -> Self {
        MbError::Other(e.into())
    }
}

/// Valide l'adresse : HTTPS obligatoire, sauf en local pour le développement.
pub fn normalize_url(raw: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(raw.trim()).map_err(|_| anyhow::anyhow!("adresse invalide"))?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        _ => anyhow::bail!("la boîte aux lettres doit utiliser HTTPS"),
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

struct Api {
    http: reqwest::Client,
    base: String,
    secret: String,
}

impl Api {
    fn new(url: &str, mailbox_id: &str, secret: &str) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("client HTTP");
        Self { http, base: format!("{url}/v1/{mailbox_id}"), secret: secret.to_string() }
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Option<reqwest::Response>, MbError> {
        let res = req.bearer_auth(&self.secret).send().await?;
        match res.status() {
            reqwest::StatusCode::GONE => Err(MbError::Gone),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            reqwest::StatusCode::UNAUTHORIZED => Err(anyhow::anyhow!("accès refusé par la boîte aux lettres").into()),
            s if !s.is_success() => Err(anyhow::anyhow!("erreur de la boîte aux lettres ({s})").into()),
            _ => Ok(Some(res)),
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, MbError> {
        match self.send(self.http.get(format!("{}{path}", self.base))).await? {
            Some(r) => Ok(Some(r.json().await?)),
            None => Ok(None),
        }
    }

    async fn put<B: Serialize>(&self, path: &str, body: &B) -> Result<(), MbError> {
        self.send(self.http.put(format!("{}{path}", self.base)).json(body)).await?;
        Ok(())
    }
}

/// Boucle principale : tourne pendant toute la vie de l'application.
pub async fn run(core: Arc<Core>) {
    let mut rev = core.group_rev.subscribe();
    let mut backoff = 1u64;
    loop {
        let group = core.group();
        let url = group.mailbox.clone().filter(|_| !core.sync_paused());
        let Some(url) = url else {
            core.set_mailbox_status(MailboxStatus::Disabled);
            let _ = post_pending_rekey(&core).await;
            tokio::select! {
                _ = rev.changed() => {}
                _ = core.kick_mailbox.notified() => {}
            }
            continue;
        };
        if !matches!(*core.mailbox_status.lock().unwrap(), MailboxStatus::Error(_)) {
            core.set_mailbox_status(MailboxStatus::Connecting);
        }
        match session(&core, &url, &group, &mut rev).await {
            Ok(()) => backoff = 1,
            Err(e) => {
                tracing::info!("boîte aux lettres indisponible : {e}");
                core.set_mailbox_status(MailboxStatus::Error(e.to_string()));
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                    _ = rev.changed() => {}
                    _ = core.kick_mailbox.notified() => {}
                }
                backoff = (backoff * 2).min(60);
            }
        }
    }
}

/// Dépose dans l'ancienne boîte la nouvelle clé chiffrée pour chaque appareil restant.
async fn post_pending_rekey(core: &Core) -> anyhow::Result<()> {
    let Some(raw) = core.with_store(|s| s.get("pending_rekey")) else { return Ok(()) };
    let p: PendingRekey = serde_json::from_str(&raw)?;
    let api = Api::new(&p.url, &p.mailbox_id, &p.secret);
    match api.put("/rekey", &p.body).await {
        Ok(()) | Err(MbError::Gone) => {
            core.with_store(|s| s.delete_kv("pending_rekey"));
            Ok(())
        }
        Err(MbError::Other(e)) => Err(e),
    }
}

/// Le groupe a été renouvelé : on récupère la nouvelle clé qui nous est destinée.
async fn follow_rekey(core: &Core, api: &Api, session_epoch: u32) -> anyhow::Result<()> {
    // Renouvellement déjà connu (par exemple fait par cet appareil) : rien à faire.
    if core.group().epoch > session_epoch {
        return Ok(());
    }
    let rekey: Option<Rekey> = api.get("/rekey").await.map_err(|_| anyhow::anyhow!("renouvellement illisible"))?;
    let mine = rekey.and_then(|r| r.wrapped.into_iter().find(|w| w.recipient == core.device_id));
    match mine.and_then(|w| core.x25519.unwrap_state(&w.blob).ok()) {
        Some(state) => {
            core.adopt(state);
        }
        None => core.become_solo("Cet appareil a été retiré du groupe."),
    }
    Ok(())
}

async fn session(core: &Arc<Core>, url: &str, group: &GroupState, rev: &mut tokio::sync::watch::Receiver<u64>) -> anyhow::Result<()> {
    let keys = GroupKeys::from_state(group)?;
    let api = Api::new(url, &keys.mailbox_id, &keys.mailbox_secret);
    post_pending_rekey(core).await?;

    let ws_url = format!("{}/ws", api.base.replacen("http", "ws", 1));
    let mut req = ws_url.into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", keys.mailbox_secret).parse()?);
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await?;

    macro_rules! sync {
        () => {
            match sync_once(core, &api, &keys).await {
                Ok(()) => {}
                Err(MbError::Gone) => return follow_rekey(core, &api, group.epoch).await,
                Err(MbError::Other(e)) => return Err(e),
            }
        };
    }

    sync!();
    core.set_mailbox_status(MailboxStatus::Online);
    let started = (group.epoch, group.key.clone(), group.mailbox.clone());

    loop {
        tokio::select! {
            msg = ws.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None => anyhow::bail!("connexion fermée"),
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(_)) => continue,
                };
                match serde_json::from_str::<MailboxEvent>(&text) {
                    Ok(MailboxEvent::Rekey) => return follow_rekey(core, &api, group.epoch).await,
                    Ok(_) => sync!(),
                    Err(_) => {}
                }
            }
            _ = core.kick_mailbox.notified() => {
                if core.sync_paused() { return Ok(()); }
                post_pending_rekey(core).await?;
                sync!();
            }
            _ = rev.changed() => {
                let g = core.group();
                if core.sync_paused() || (g.epoch, g.key.clone(), g.mailbox.clone()) != started {
                    return Ok(());
                }
                sync!();
            }
        }
    }
}

async fn sync_once(core: &Core, api: &Api, keys: &GroupKeys) -> Result<(), MbError> {
    // 1. État du groupe : on adopte le plus récent, on publie le nôtre s'il l'est.
    let remote: Option<StateBlob> = api.get("/state").await?;
    if let Some(s) = &remote {
        if let Ok(state) = crypto::decrypt_state(&keys.enc_key, &s.blob) {
            if core.adopt(state) && core.keys().mailbox_id != keys.mailbox_id {
                return Ok(()); // nouvelle clé : la session redémarre
            }
        }
    }
    let mine = core.group();
    if remote.as_ref().map_or(true, |s| s.version < mine.version) {
        api.put("/state", &StateBlob { version: mine.version, blob: crypto::encrypt_state(&keys.enc_key, &mine) }).await?;
    }

    // 2. Envoi de ce qui n'y est pas encore.
    loop {
        let rows = core.with_store(|s| s.unsynced(200)).map_err(anyhow::Error::from)?;
        if rows.is_empty() {
            break;
        }
        // Lots bornés en octets (images) : la boîte refuse les requêtes de plus de 16 Mo.
        let mut size = 0;
        let records: Vec<Record> = rows
            .iter()
            .map(|r| core.record_of(r, keys))
            .take_while(|r| {
                let first = size == 0;
                size += r.blob.as_ref().map_or(0, |b| b.len()) + 100;
                first || size <= PUSH_BYTES
            })
            .collect();
        let rows = &rows[..records.len()];
        api.put("/records", &PushRecords { records }).await?;
        let ids: Vec<Uuid> = rows.iter().map(|r| r.item.id).collect();
        core.with_store(|s| s.mark_synced(&ids)).map_err(anyhow::Error::from)?;
    }

    // 3. Réception depuis le dernier numéro connu.
    let seq_key = format!("mailbox_seq:{}", keys.mailbox_id);
    loop {
        let since: i64 = core.with_store(|s| s.get(&seq_key)).and_then(|v| v.parse().ok()).unwrap_or(0);
        let Some(page) = api.get::<PullResponse>(&format!("/records?since={since}")).await? else { break };
        let records: Vec<Record> = page.records.into_iter().map(|r| r.record).collect();
        core.apply_records(&records, true);
        core.with_store(|s| s.set(&seq_key, &page.max_seq.to_string()));
        if !page.more {
            break;
        }
    }
    Ok(())
}
