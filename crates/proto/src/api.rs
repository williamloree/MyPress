//! Formats échangés entre appareils (P2P) et avec la boîte aux lettres optionnelle.
//!
//! Le contenu du presse-papiers circule toujours chiffré avec la clé du groupe
//! (voir [`crate::crypto`]) : ni les relais P2P ni la boîte aux lettres ne peuvent le lire.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const HISTORY_LIMITS: [u32; 5] = [50, 100, 250, 500, 1000];
pub const DEFAULT_HISTORY_LIMIT: u32 = 100;
/// Taille max d'un blob chiffré (base64).
pub const MAX_BLOB_LEN: usize = 1024 * 1024;

/// Un élément d'historique chiffré. `blob == None` signifie « supprimé ».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: Uuid,
    pub created_at: i64,
    pub blob: Option<String>,
}

/// Contenu en clair d'un élément, sérialisé puis chiffré dans `Record::blob`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemPayload {
    pub kind: ContentKind,
    pub text: String,
    pub device_name: String,
    /// Identifiant de l'appareil d'origine.
    pub device_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Text,
    Url,
    Code,
}

impl ContentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentKind::Text => "text",
            ContentKind::Url => "url",
            ContentKind::Code => "code",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "url" => ContentKind::Url,
            "code" => ContentKind::Code,
            _ => ContentKind::Text,
        }
    }

    /// Classification heuristique, uniquement pour l'affichage.
    pub fn detect(text: &str) -> Self {
        let t = text.trim();
        if !t.contains(char::is_whitespace)
            && (t.starts_with("http://") || t.starts_with("https://") || t.starts_with("www."))
        {
            return ContentKind::Url;
        }
        let code_markers = ["{", "}", "=>", "();", "fn ", "def ", "function ", "#include", "</"];
        if t.contains('\n') && code_markers.iter().filter(|m| t.contains(*m)).count() >= 2 {
            return ContentKind::Code;
        }
        ContentKind::Text
    }
}

/// Appareil membre du groupe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    /// Identifiant iroh (clé publique Ed25519), qui authentifie les connexions P2P.
    pub id: String,
    pub name: String,
    /// Clé publique X25519 (base64), pour lui transmettre une nouvelle clé de groupe.
    pub x25519: String,
    pub added_at: i64,
}

/// État partagé du groupe d'appareils. La version la plus élevée l'emporte ;
/// `epoch` change à chaque renouvellement de clé (révocation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupState {
    pub epoch: u32,
    pub version: u64,
    /// Clé du groupe (base64, 32 octets). Ne circule que chiffrée.
    pub key: String,
    pub members: Vec<Member>,
    pub history_limit: u32,
    /// Adresse de la boîte aux lettres optionnelle.
    pub mailbox: Option<String>,
}

impl GroupState {
    pub fn rank(&self) -> (u32, u64) {
        (self.epoch, self.version)
    }

    pub fn is_member(&self, id: &str) -> bool {
        self.members.iter().any(|m| m.id == id)
    }
}

// ---------- P2P ----------

/// Messages échangés sur une connexion iroh (trames JSON préfixées par leur longueur).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PeerMsg {
    /// Demande d'entrée dans le groupe avec un code d'invitation.
    Join { token: String, member: Member },
    /// Réponse à `Join` : état complet du groupe.
    Welcome { state: GroupState },
    Denied { reason: String },
    /// Résumé envoyé à l'ouverture : rang du groupe et éléments connus (`true` = supprimé).
    Hello { epoch: u32, version: u64, ids: Vec<(Uuid, bool)> },
    /// Nouvel état du groupe (envoyé à un membre en retard).
    Group { state: GroupState },
    Records { records: Vec<Record> },
}

// ---------- Boîte aux lettres ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxRecord {
    pub seq: i64,
    #[serde(flatten)]
    pub record: Record,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushRecords {
    pub records: Vec<Record>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullResponse {
    pub records: Vec<MailboxRecord>,
    pub max_seq: i64,
    pub more: bool,
}

/// État du groupe chiffré avec la clé du groupe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateBlob {
    pub version: u64,
    pub blob: String,
}

/// Nouvelle clé de groupe, chiffrée pour chaque membre restant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rekey {
    pub wrapped: Vec<WrappedKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedKey {
    pub recipient: String,
    pub blob: String,
}

/// Notifications envoyées par la boîte aux lettres sur le WebSocket.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MailboxEvent {
    Changed { seq: i64 },
    State { version: u64 },
    Rekey,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_kinds() {
        assert_eq!(ContentKind::detect("https://example.com/a?b=c"), ContentKind::Url);
        assert_eq!(ContentKind::detect("voir https://example.com"), ContentKind::Text);
        assert_eq!(ContentKind::detect("fn main() {\n    println!();\n}"), ContentKind::Code);
        assert_eq!(ContentKind::detect("bonjour\nle monde"), ContentKind::Text);
    }
}
