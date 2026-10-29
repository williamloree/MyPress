//! Groupes de partage et transfert de fichiers, à part de la synchronisation du presse-papiers.
//!
//! Un groupe de partage réunit des appareils de plusieurs personnes, qui s'envoient
//! explicitement des fichiers. Les fichiers peuvent aussi être envoyés à ses propres
//! appareils (destination [`ME`], le groupe d'appareils du presse-papiers).
//!
//! Chaque envoi passe par une connexion iroh dédiée (ALPN [`ALPN`]), chiffrée et
//! authentifiée par l'identité des appareils. Un flux par message : trame JSON
//! (voir `p2p`), suivie pour un fichier de son contenu brut, puis réponse `Ok`.
//! Rien n'est stocké en route : un membre éteint ne reçoit pas l'envoi.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::{
    core::{parse_code, Core},
    p2p::{read_msg, write_msg},
    store::now_ms,
};

pub const ALPN: &[u8] = b"mypress/share/1";
/// Destination « mes appareils ».
pub const ME: &str = "me";
const CODE_PREFIX: &str = "MPG1-";
const MAX_FILE: u64 = 4 * 1024 * 1024 * 1024;
const MAX_TRANSFERS: usize = 50;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShareMember {
    pub id: String,
    pub name: String,
}

/// Groupe de partage. La version la plus élevée l'emporte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShareGroup {
    pub id: String,
    pub name: String,
    pub version: u64,
    pub members: Vec<ShareMember>,
}

impl ShareGroup {
    fn has(&self, id: &str) -> bool {
        self.members.iter().any(|m| m.id == id)
    }
}

/// Fichier envoyé ou reçu, pour l'affichage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    pub id: Uuid,
    pub created_at: i64,
    pub incoming: bool,
    /// Nom du groupe.
    pub group: String,
    /// Expéditeur, ou résumé des destinataires.
    pub peer: String,
    pub name: String,
    pub size: u64,
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Msg {
    Join { token: String, name: String },
    Welcome { group: ShareGroup },
    Group { group: ShareGroup },
    /// Suivi de `size` octets.
    File { group: String, name: String, size: u64 },
    Ok,
    Denied { reason: String },
}

pub struct Share {
    core: Arc<Core>,
    /// Dossier de réception.
    pub dir: PathBuf,
    /// Codes d'invitation valides : secret → (groupe, expiration).
    invites: Mutex<HashMap<String, (String, i64)>>,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Share {
    pub fn new(core: Arc<Core>, dir: PathBuf, notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self { core, dir, invites: Mutex::new(HashMap::new()), notify: Box::new(notify) }
    }

    // ---------- stockage ----------

    pub fn groups(&self) -> Vec<ShareGroup> {
        self.core.with_store(|s| s.get("share_groups")).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
    }

    fn group(&self, id: &str) -> Option<ShareGroup> {
        self.groups().into_iter().find(|g| g.id == id)
    }

    /// Remplace (ou retire avec `None`) un groupe.
    fn put_group(&self, id: &str, group: Option<ShareGroup>) {
        let mut groups = self.groups();
        groups.retain(|g| g.id != id);
        groups.extend(group);
        self.core.with_store(|s| s.set("share_groups", &serde_json::to_string(&groups).unwrap()));
        (self.notify)();
    }

    pub fn transfers(&self) -> Vec<Transfer> {
        self.core.with_store(|s| s.get("transfers")).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
    }

    fn add_transfer(&self, t: Transfer) {
        let mut list = self.transfers();
        list.insert(0, t);
        list.truncate(MAX_TRANSFERS);
        self.core.with_store(|s| s.set("transfers", &serde_json::to_string(&list).unwrap()));
        (self.notify)();
    }

    fn me(&self) -> ShareMember {
        ShareMember { id: self.core.device_id.clone(), name: self.core.device_name.clone() }
    }

    /// Membres d'une destination, sauf cet appareil, et nom de la destination.
    fn recipients(&self, target: &str) -> anyhow::Result<(String, Vec<ShareMember>)> {
        let (name, members) = if target == ME {
            let members = self.core.group().members.into_iter().map(|m| ShareMember { id: m.id, name: m.name }).collect();
            ("Mes appareils".to_string(), members)
        } else {
            let g = self.group(target).ok_or_else(|| anyhow::anyhow!("groupe introuvable"))?;
            (g.name, g.members)
        };
        Ok((name, members.into_iter().filter(|m| m.id != self.core.device_id).collect()))
    }

    // ---------- groupes ----------

    pub fn create_group(&self, name: &str) -> anyhow::Result<ShareGroup> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("donnez un nom au groupe");
        }
        let g = ShareGroup { id: Uuid::now_v7().to_string(), name: name.into(), version: 0, members: vec![self.me()] };
        self.put_group(&g.id, Some(g.clone()));
        Ok(g)
    }

    /// Code d'invitation à usage unique, valable 15 minutes.
    pub fn invite(&self, group_id: &str) -> anyhow::Result<String> {
        self.group(group_id).ok_or_else(|| anyhow::anyhow!("groupe introuvable"))?;
        let token = mypress_proto::crypto::random_key()[..16].to_vec();
        let mut invites = self.invites.lock().unwrap();
        invites.retain(|_, (_, exp)| *exp > now_ms());
        invites.insert(mypress_proto::crypto::b64(&token), (group_id.into(), now_ms() + 15 * 60 * 1000));
        Ok(self.core.invite_code(CODE_PREFIX, &token))
    }

    pub async fn join(&self, code: &str) -> anyhow::Result<ShareGroup> {
        let (eid, token) = parse_code(code, CODE_PREFIX)?;
        if eid.to_string() == self.core.device_id {
            anyhow::bail!("ce code vient de cet appareil");
        }
        let conn = self.connect(&eid.to_string()).await.map_err(|_| anyhow::anyhow!("l'autre appareil ne répond pas (est-il allumé ?)"))?;
        match request(&conn, &Msg::Join { token, name: self.core.device_name.clone() }).await? {
            Msg::Welcome { group } if group.has(&self.core.device_id) => {
                conn.close(0u32.into(), b"fin");
                self.put_group(&group.id.clone(), Some(group.clone()));
                Ok(group)
            }
            Msg::Denied { reason } => anyhow::bail!(reason),
            _ => anyhow::bail!("réponse inattendue de l'autre appareil"),
        }
    }

    /// Quitte un groupe et prévient les membres joignables.
    pub fn leave(self: &Arc<Self>, group_id: &str) {
        let Some(mut g) = self.group(group_id) else { return };
        g.members.retain(|m| m.id != self.core.device_id);
        g.version += 1;
        self.put_group(group_id, None);
        self.push_group(g, None);
    }

    /// Envoie le nouvel état du groupe aux autres membres, en tâche de fond.
    fn push_group(self: &Arc<Self>, group: ShareGroup, except: Option<&str>) {
        for m in &group.members {
            if m.id == self.core.device_id || Some(m.id.as_str()) == except {
                continue;
            }
            let (this, id, group) = (self.clone(), m.id.clone(), group.clone());
            tokio::spawn(async move {
                let res = async {
                    let conn = this.connect(&id).await?;
                    request(&conn, &Msg::Group { group }).await?;
                    conn.close(0u32.into(), b"fin");
                    anyhow::Ok(())
                };
                if let Err(e) = res.await {
                    tracing::debug!("groupe de partage non transmis à {id} : {e}");
                }
            });
        }
    }

    /// Adopte un état reçu d'un membre connu, s'il est plus récent.
    fn adopt(&self, from: &str, group: ShareGroup) {
        let Some(local) = self.group(&group.id) else { return };
        if !local.has(from) || group.version <= local.version {
            return;
        }
        let keep = group.has(&self.core.device_id);
        self.put_group(&group.id.clone(), keep.then_some(group));
    }

    // ---------- fichiers ----------

    /// Envoie des fichiers à tous les membres joignables d'une destination.
    /// Retourne (destinataires atteints, destinataires).
    pub async fn send_files(&self, target: &str, paths: &[PathBuf]) -> anyhow::Result<(usize, usize)> {
        let (group_name, members) = self.recipients(target)?;
        if members.is_empty() {
            anyhow::bail!("aucun autre appareil dans « {group_name} »");
        }
        let mut files = Vec::new();
        for p in paths {
            let meta = tokio::fs::metadata(p).await?;
            if !meta.is_file() {
                anyhow::bail!("{} n'est pas un fichier (dossiers non pris en charge)", p.display());
            }
            if meta.len() > MAX_FILE {
                anyhow::bail!("{} dépasse 4 Go", p.display());
            }
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "fichier".into());
            files.push((p.clone(), name, meta.len()));
        }
        let sends = members.iter().map(|m| async {
            let res = self.send_to(&m.id, target, &files).await;
            if let Err(e) = &res {
                tracing::info!("envoi à {} impossible : {e}", m.name);
            }
            res.is_ok()
        });
        let ok = futures_util::future::join_all(sends).await.into_iter().filter(|ok| *ok).count();
        for (path, name, size) in &files {
            self.add_transfer(Transfer {
                id: Uuid::now_v7(),
                created_at: now_ms(),
                incoming: false,
                group: group_name.clone(),
                peer: format!("{ok}/{} destinataire{}", members.len(), if members.len() > 1 { "s" } else { "" }),
                name: name.clone(),
                size: *size,
                path: path.display().to_string(),
            });
        }
        Ok((ok, members.len()))
    }

    async fn send_to(&self, id: &str, target: &str, files: &[(PathBuf, String, u64)]) -> anyhow::Result<()> {
        let conn = self.connect(id).await?;
        for (path, name, size) in files {
            let (mut send, mut recv) = conn.open_bi().await?;
            write_msg(&mut send, &Msg::File { group: target.into(), name: name.clone(), size: *size }).await?;
            let mut file = tokio::fs::File::open(path).await?.take(*size);
            tokio::io::copy(&mut file, &mut send).await?;
            send.finish()?;
            match read_msg::<Msg>(&mut recv).await? {
                Some(Msg::Ok) => {}
                Some(Msg::Denied { reason }) => anyhow::bail!(reason),
                _ => anyhow::bail!("réponse inattendue"),
            }
        }
        conn.close(0u32.into(), b"fin");
        Ok(())
    }

    // ---------- réception ----------

    async fn connect(&self, id: &str) -> anyhow::Result<Connection> {
        let endpoint = self.core.endpoint.get().ok_or_else(|| anyhow::anyhow!("réseau non démarré"))?.clone();
        let eid: iroh::EndpointId = id.parse()?;
        Ok(tokio::time::timeout(TIMEOUT, endpoint.connect(eid, ALPN)).await??)
    }

    /// Connexion entrante sur [`ALPN`] : un message par flux, jusqu'à la fermeture.
    pub async fn accept(self: Arc<Self>, conn: Connection) {
        let remote = conn.remote_id().to_string();
        while let Ok((send, recv)) = conn.accept_bi().await {
            let (this, remote) = (self.clone(), remote.clone());
            tokio::spawn(async move {
                if let Err(e) = this.handle(&remote, send, recv).await {
                    tracing::debug!("partage refusé de {remote} : {e}");
                }
            });
        }
    }

    async fn handle(self: Arc<Self>, remote: &str, mut send: SendStream, mut recv: RecvStream) -> anyhow::Result<()> {
        let reply = match read_msg::<Msg>(&mut recv).await? {
            Some(Msg::Join { token, name }) => {
                let invite = self.invites.lock().unwrap().remove(&token);
                match invite.filter(|(_, exp)| *exp > now_ms()).and_then(|(id, _)| self.group(&id)) {
                    Some(mut g) => {
                        g.members.retain(|m| m.id != remote);
                        g.members.push(ShareMember { id: remote.into(), name });
                        g.version += 1;
                        self.put_group(&g.id.clone(), Some(g.clone()));
                        self.push_group(g.clone(), Some(remote));
                        Msg::Welcome { group: g }
                    }
                    None => Msg::Denied { reason: "code d'invitation invalide ou expiré".into() },
                }
            }
            Some(Msg::Group { group }) => {
                self.adopt(remote, group);
                Msg::Ok
            }
            Some(Msg::File { group, name, size }) => match self.sender_name(remote, &group) {
                Some((group_name, peer)) if size <= MAX_FILE => {
                    let path = self.receive(&name, size, &mut recv).await?;
                    self.add_transfer(Transfer {
                        id: Uuid::now_v7(),
                        created_at: now_ms(),
                        incoming: true,
                        group: group_name,
                        peer,
                        name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                        size,
                        path: path.display().to_string(),
                    });
                    Msg::Ok
                }
                _ => Msg::Denied { reason: "envoi refusé".into() },
            },
            _ => anyhow::bail!("message inattendu"),
        };
        write_msg(&mut send, &reply).await?;
        send.finish()?;
        // Laisse la réponse partir avant la fermeture du flux.
        let _ = tokio::time::timeout(TIMEOUT, send.stopped()).await;
        Ok(())
    }

    /// (nom du groupe, nom de l'expéditeur) si l'expéditeur fait partie de la destination.
    fn sender_name(&self, remote: &str, target: &str) -> Option<(String, String)> {
        let (group, members) = if target == ME {
            let g = self.core.group();
            ("Mes appareils".to_string(), g.members.into_iter().map(|m| ShareMember { id: m.id, name: m.name }).collect())
        } else {
            let g = self.group(target)?;
            (g.name, g.members)
        };
        members.into_iter().find(|m| m.id == remote).map(|m| (group, m.name))
    }

    /// Écrit le contenu reçu dans un fichier temporaire, renommé une fois complet.
    async fn receive(&self, name: &str, size: u64, recv: &mut RecvStream) -> anyhow::Result<PathBuf> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let final_path = unique_path(&self.dir, &safe_name(name));
        let mut part = final_path.clone().into_os_string();
        part.push(".part");
        let part = PathBuf::from(part);
        let mut file = tokio::fs::File::create(&part).await?;
        let n = tokio::io::copy(&mut recv.take(size), &mut file).await;
        drop(file);
        if n.as_ref().ok() != Some(&size) {
            let _ = tokio::fs::remove_file(&part).await;
            anyhow::bail!("fichier incomplet");
        }
        tokio::fs::rename(&part, &final_path).await?;
        Ok(final_path)
    }
}

/// Envoie un message sur un nouveau flux et attend la réponse.
async fn request(conn: &Connection, msg: &Msg) -> anyhow::Result<Msg> {
    let (mut send, mut recv) = conn.open_bi().await?;
    write_msg(&mut send, msg).await?;
    send.finish()?;
    tokio::time::timeout(TIMEOUT, read_msg(&mut recv)).await??.ok_or_else(|| anyhow::anyhow!("pas de réponse"))
}

/// Nom de fichier sans chemin ni caractère interdit sous Windows.
fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().map(|c| if "<>:\"|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    let clean = clean.trim_matches([' ', '.']);
    if clean.is_empty() { "fichier".into() } else { clean.into() }
}

/// `nom (2).ext`, `nom (3).ext`… si le fichier existe déjà.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let p = Path::new(name);
    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sanitized() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("..\\..\\Windows\\a:b?.txt"), "a_b_.txt");
        assert_eq!(safe_name(".."), "fichier");
        assert_eq!(safe_name("rapport final.pdf"), "rapport final.pdf");
    }
}
