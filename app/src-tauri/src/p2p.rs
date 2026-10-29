//! Synchronisation directe entre appareils (iroh : QUIC chiffré, connexion directe
//! si possible, relais public sinon). Chaque appareil tente de joindre tous les
//! membres du groupe ; seuls les membres sont acceptés, sauf pour un appairage.
//!
//! Protocole : un flux bidirectionnel par connexion, trames JSON préfixées par leur
//! longueur. Chaque côté envoie `Hello` (résumé de son historique) ; l'autre répond
//! avec ce qui manque. Ensuite, chaque modification locale est diffusée aussitôt.

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use iroh::{
    endpoint::{Connection, RecvStream, SendStream},
    Endpoint, EndpointId,
};
use tokio::sync::mpsc;

use mypress_proto::api::{PeerMsg, Record};

use crate::{
    core::{parse_invite, Core, Peer},
    share::{self, Share},
};

pub const ALPN: &[u8] = b"mypress/1";
const MAX_FRAME: usize = 16 * 1024 * 1024;
/// Taille cible d'un lot d'éléments dans une trame.
const BATCH_BYTES: usize = 4 * 1024 * 1024;
const DIAL_INTERVAL: Duration = Duration::from_secs(20);
const TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) async fn write_msg(send: &mut SendStream, msg: &impl serde::Serialize) -> anyhow::Result<()> {
    let data = serde_json::to_vec(msg)?;
    send.write_all(&(data.len() as u32).to_be_bytes()).await?;
    send.write_all(&data).await?;
    Ok(())
}

/// `None` : le flux a été fermé proprement.
pub(crate) async fn read_msg<T: serde::de::DeserializeOwned>(recv: &mut RecvStream) -> anyhow::Result<Option<T>> {
    let mut len = [0u8; 4];
    if recv.read_exact(&mut len).await.is_err() {
        return Ok(None);
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        anyhow::bail!("trame trop grande");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(Some(serde_json::from_slice(&buf)?))
}

/// Découpe une liste d'éléments en trames de taille raisonnable.
fn batches(records: Vec<Record>) -> Vec<PeerMsg> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut size = 0;
    for r in records {
        size += r.blob.as_ref().map_or(0, |b| b.len()) + 100;
        cur.push(r);
        if size >= BATCH_BYTES {
            out.push(PeerMsg::Records { records: std::mem::take(&mut cur) });
            size = 0;
        }
    }
    if !cur.is_empty() {
        out.push(PeerMsg::Records { records: cur });
    }
    out
}

/// `share` : groupes de partage et fichiers (absent dans les outils de test en ligne de commande).
pub fn start(core: Arc<Core>, endpoint: Endpoint, share: Option<Arc<Share>>) {
    let _ = core.endpoint.set(endpoint.clone());
    tokio::spawn(accept_loop(core.clone(), endpoint.clone(), share));
    tokio::spawn(dial_loop(core, endpoint));
}

async fn accept_loop(core: Arc<Core>, endpoint: Endpoint, share: Option<Arc<Share>>) {
    while let Some(incoming) = endpoint.accept().await {
        let (core, share) = (core.clone(), share.clone());
        tokio::spawn(async move {
            if let Err(e) = accept_one(core, share, incoming).await {
                tracing::debug!("connexion entrante refusée : {e}");
            }
        });
    }
}

async fn accept_one(core: Arc<Core>, share: Option<Arc<Share>>, incoming: iroh::endpoint::Incoming) -> anyhow::Result<()> {
    let conn = incoming.await?;
    // Partage de fichiers : indépendant de la pause de la synchronisation.
    if conn.alpn() == share::ALPN {
        if let Some(share) = share {
            share.accept(conn).await;
        }
        return Ok(());
    }
    if core.sync_paused() {
        conn.close(0u32.into(), b"pause");
        return Ok(());
    }
    let remote = conn.remote_id().to_string();
    let (mut send, mut recv) = tokio::time::timeout(TIMEOUT, conn.accept_bi()).await??;
    let first = tokio::time::timeout(TIMEOUT, read_msg(&mut recv)).await??;
    match first {
        Some(PeerMsg::Join { token, member }) => {
            // L'identité annoncée doit être celle prouvée par la connexion.
            if member.id != remote || !core.consume_invite(&token) {
                write_msg(&mut send, &PeerMsg::Denied { reason: "code d'invitation invalide ou expiré".into() }).await?;
                let _ = send.finish();
                conn.closed().await;
                return Ok(());
            }
            tracing::info!("nouvel appareil : {}", member.name);
            let state = core.update_group(|g| {
                g.members.retain(|m| m.id != member.id);
                g.members.push(member);
            });
            write_msg(&mut send, &PeerMsg::Welcome { state }).await?;
            session(core, conn, send, recv, remote, None).await;
        }
        Some(msg) if core.group().is_member(&remote) => session(core, conn, send, recv, remote, Some(msg)).await,
        other => {
            tracing::debug!("refus de {remote} : {:?}", other.map(|m| std::mem::discriminant(&m)));
            conn.close(1u32.into(), b"non membre")
        }
    }
    Ok(())
}

async fn dial_loop(core: Arc<Core>, endpoint: Endpoint) {
    let dialing = Arc::new(std::sync::Mutex::new(HashSet::<String>::new()));
    let mut rev = core.group_rev.subscribe();
    loop {
        if !core.sync_paused() {
            let online = core.online_members();
            for m in core.group().members {
                if m.id == core.device_id || online.contains(&m.id) || !dialing.lock().unwrap().insert(m.id.clone()) {
                    continue;
                }
                let (core, endpoint, dialing) = (core.clone(), endpoint.clone(), dialing.clone());
                tokio::spawn(async move {
                    if let Err(e) = dial(core, endpoint, &m.id, &dialing).await {
                        tracing::debug!("{} injoignable : {e}", m.name);
                    }
                    dialing.lock().unwrap().remove(&m.id);
                });
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(DIAL_INTERVAL) => {}
            _ = rev.changed() => {}
        }
    }
}

async fn dial(core: Arc<Core>, endpoint: Endpoint, id: &str, dialing: &std::sync::Mutex<HashSet<String>>) -> anyhow::Result<()> {
    let eid: EndpointId = id.parse()?;
    tracing::debug!("{} : tentative vers {id}", core.device_name);
    let conn = tokio::time::timeout(TIMEOUT, endpoint.connect(eid, ALPN)).await??;
    let (send, recv) = conn.open_bi().await?;
    dialing.lock().unwrap().remove(id);
    session(core, conn, send, recv, id.to_string(), None).await;
    Ok(())
}

/// Rejoint le groupe d'un autre appareil à partir de son code d'invitation.
pub async fn join(core: Arc<Core>, code: &str) -> anyhow::Result<()> {
    let (eid, token) = parse_invite(code)?;
    if eid.to_string() == core.device_id {
        anyhow::bail!("ce code vient de cet appareil");
    }
    let endpoint = core.endpoint.get().ok_or_else(|| anyhow::anyhow!("réseau non démarré"))?.clone();
    let conn = tokio::time::timeout(TIMEOUT, endpoint.connect(eid, ALPN))
        .await
        .map_err(|_| anyhow::anyhow!("l'autre appareil ne répond pas (est-il allumé ?)"))??;
    let (mut send, mut recv) = conn.open_bi().await?;
    write_msg(&mut send, &PeerMsg::Join { token, member: core.me() }).await?;
    match tokio::time::timeout(TIMEOUT, read_msg(&mut recv)).await?? {
        Some(PeerMsg::Welcome { state }) if state.is_member(&core.device_id) => {
            core.join(state);
            tokio::spawn(session(core, conn, send, recv, eid.to_string(), None));
            Ok(())
        }
        Some(PeerMsg::Denied { reason }) => anyhow::bail!(reason),
        _ => anyhow::bail!("réponse inattendue de l'autre appareil"),
    }
}

/// Échange avec un membre jusqu'à la fermeture de la connexion.
async fn session(core: Arc<Core>, conn: Connection, mut send: SendStream, mut recv: RecvStream, member: String, first: Option<PeerMsg>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<PeerMsg>();
    let key = conn.stable_id();
    core.peers.lock().unwrap().insert(key, Peer { member: member.clone(), tx: tx.clone(), conn: conn.clone() });
    core.status_changed();
    tracing::debug!("{} : session ouverte avec {member}", core.device_name);

    // Époque du dernier `Hello` envoyé sur cette connexion.
    let sent_epoch = Arc::new(AtomicU64::new(u64::MAX));
    let sent = sent_epoch.clone();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let PeerMsg::Hello { epoch, .. } = &msg {
                sent.store(*epoch as u64, Ordering::Relaxed);
            }
            if write_msg(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });
    let _ = tx.send(core.hello());
    if let Some(msg) = first {
        handle(&core, &tx, &sent_epoch, msg);
    }
    loop {
        match read_msg(&mut recv).await {
            Ok(Some(msg)) if core.group().is_member(&member) => handle(&core, &tx, &sent_epoch, msg),
            _ => break,
        }
    }

    core.peers.lock().unwrap().remove(&key);
    writer.abort();
    conn.close(0u32.into(), b"fin");
    core.status_changed();
    tracing::debug!("{} : session terminée avec {member}", core.device_name);
}

fn handle(core: &Core, tx: &mpsc::UnboundedSender<PeerMsg>, sent_epoch: &AtomicU64, msg: PeerMsg) {
    match msg {
        PeerMsg::Hello { epoch, version, ids } => {
            let g = core.group();
            if (epoch, version) < g.rank() {
                let _ = tx.send(PeerMsg::Group { state: g.clone() });
            }
            // Même clé : on envoie ce qui lui manque.
            if epoch == g.epoch {
                for batch in batches(core.missing_for(&ids)) {
                    let _ = tx.send(batch);
                }
                // Notre dernier résumé date d'une autre clé : il l'a ignoré, on le renvoie.
                if sent_epoch.load(Ordering::Relaxed) != g.epoch as u64 {
                    let _ = tx.send(core.hello());
                }
            }
        }
        PeerMsg::Group { state } => {
            core.adopt(state);
        }
        PeerMsg::Records { records } => core.apply_records(&records, false),
        // Un membre nous signale que nous avons été retirés du groupe.
        PeerMsg::Denied { .. } => core.become_solo("Cet appareil a été retiré du groupe."),
        PeerMsg::Join { .. } | PeerMsg::Welcome { .. } => {}
    }
}
