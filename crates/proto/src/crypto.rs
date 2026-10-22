//! Chiffrement de bout en bout.
//!
//! Chaque groupe d'appareils partage une clé aléatoire de 32 octets. Par HKDF on en dérive :
//! - la clé de chiffrement des éléments (XChaCha20-Poly1305) ;
//! - l'identifiant et le secret d'accès de la boîte aux lettres, qui ne permettent pas de déchiffrer.
//!
//! Chaque appareil possède aussi une paire X25519 : lors d'une révocation, la nouvelle clé
//! de groupe est chiffrée séparément pour chaque appareil restant.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    XChaCha20Poly1305, XNonce,
};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use uuid::Uuid;
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

use crate::api::{GroupState, ItemPayload};

const NONCE_LEN: usize = 24;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("clé invalide")]
    Key,
    #[error("données chiffrées invalides")]
    Invalid,
}

pub fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    OsRng.fill_bytes(&mut k);
    k
}

pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

pub fn key_from_b64(s: &str) -> Result<[u8; 32], CryptoError> {
    B64.decode(s).ok().and_then(|v| v.try_into().ok()).ok_or(CryptoError::Key)
}

/// Clés dérivées de la clé du groupe.
#[derive(Clone)]
pub struct GroupKeys {
    pub enc_key: [u8; 32],
    /// Identifiant public de la boîte aux lettres (hexadécimal).
    pub mailbox_id: String,
    /// Secret d'accès à la boîte aux lettres (base64).
    pub mailbox_secret: String,
}

impl GroupKeys {
    pub fn derive(group_key: &[u8; 32]) -> Self {
        let hk = Hkdf::<Sha256>::new(None, group_key);
        let expand = |info: &[u8]| {
            let mut out = [0u8; 32];
            hk.expand(info, &mut out).expect("longueur HKDF valide");
            out
        };
        let id = expand(b"mypress mailbox-id v1");
        Self {
            enc_key: expand(b"mypress enc v1"),
            mailbox_id: id.iter().map(|b| format!("{b:02x}")).collect(),
            mailbox_secret: b64(&expand(b"mypress mailbox-auth v1")),
        }
    }

    pub fn from_state(state: &GroupState) -> Result<Self, CryptoError> {
        Ok(Self::derive(&key_from_b64(&state.key)?))
    }
}

fn seal(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> String {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, Payload { msg: plain, aad }).expect("chiffrement");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    B64.encode(out)
}

fn open(key: &[u8; 32], aad: &[u8], blob: &str) -> Result<Vec<u8>, CryptoError> {
    let raw = B64.decode(blob).map_err(|_| CryptoError::Invalid)?;
    if raw.len() <= NONCE_LEN {
        return Err(CryptoError::Invalid);
    }
    let (nonce, ct) = raw.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| CryptoError::Invalid)
}

/// L'identifiant de l'élément sert de données associées : un blob ne peut pas être déplacé vers un autre élément.
pub fn encrypt_item(key: &[u8; 32], id: Uuid, payload: &ItemPayload) -> String {
    seal(key, id.as_bytes(), &serde_json::to_vec(payload).expect("sérialisable"))
}

pub fn decrypt_item(key: &[u8; 32], id: Uuid, blob: &str) -> Result<ItemPayload, CryptoError> {
    serde_json::from_slice(&open(key, id.as_bytes(), blob)?).map_err(|_| CryptoError::Invalid)
}

pub fn encrypt_state(key: &[u8; 32], state: &GroupState) -> String {
    seal(key, b"mypress state v1", &serde_json::to_vec(state).expect("sérialisable"))
}

pub fn decrypt_state(key: &[u8; 32], blob: &str) -> Result<GroupState, CryptoError> {
    serde_json::from_slice(&open(key, b"mypress state v1", blob)?).map_err(|_| CryptoError::Invalid)
}

/// Paire X25519 d'un appareil.
pub struct DeviceKey(StaticSecret);

impl DeviceKey {
    pub fn generate() -> Self {
        Self(StaticSecret::random_from_rng(OsRng))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(StaticSecret::from(bytes))
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn public_b64(&self) -> String {
        b64(PublicKey::from(&self.0).as_bytes())
    }

    /// Déchiffre un état de groupe envoyé par [`wrap_state`].
    pub fn unwrap_state(&self, blob: &str) -> Result<GroupState, CryptoError> {
        let raw = B64.decode(blob).map_err(|_| CryptoError::Invalid)?;
        if raw.len() < 32 {
            return Err(CryptoError::Invalid);
        }
        let eph: [u8; 32] = raw[..32].try_into().unwrap();
        let shared = self.0.diffie_hellman(&PublicKey::from(eph));
        let key = wrap_key(shared.as_bytes(), &eph);
        let plain = open(&key, b"mypress wrap v1", &B64.encode(&raw[32..]))?;
        serde_json::from_slice(&plain).map_err(|_| CryptoError::Invalid)
    }
}

fn wrap_key(shared: &[u8; 32], eph: &[u8; 32]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(eph), shared);
    let mut key = [0u8; 32];
    hk.expand(b"mypress wrap v1", &mut key).expect("longueur HKDF valide");
    key
}

/// Chiffre l'état du groupe pour un seul appareil (clé éphémère + X25519).
pub fn wrap_state(recipient_b64: &str, state: &GroupState) -> Result<String, CryptoError> {
    let recipient = PublicKey::from(key_from_b64(recipient_b64)?);
    let eph = EphemeralSecret::random_from_rng(OsRng);
    let eph_pub = PublicKey::from(&eph);
    let shared = eph.diffie_hellman(&recipient);
    let key = wrap_key(shared.as_bytes(), eph_pub.as_bytes());
    let sealed = B64.decode(seal(&key, b"mypress wrap v1", &serde_json::to_vec(state).expect("sérialisable"))).unwrap();
    let mut out = eph_pub.as_bytes().to_vec();
    out.extend_from_slice(&sealed);
    Ok(B64.encode(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ContentKind;

    fn payload() -> ItemPayload {
        ItemPayload {
            kind: ContentKind::Text,
            text: "ligne 1\nligne 2 — é\t{}".into(),
            device_name: "PC fixe".into(),
            device_id: "abc".into(),
        }
    }

    fn state(key: &[u8; 32]) -> GroupState {
        GroupState { epoch: 1, version: 3, key: b64(key), members: vec![], history_limit: 100, mailbox: None }
    }

    #[test]
    fn group_keys_are_deterministic_and_separated() {
        let k = random_key();
        let a = GroupKeys::derive(&k);
        let b = GroupKeys::derive(&k);
        assert_eq!(a.enc_key, b.enc_key);
        assert_eq!(a.mailbox_id, b.mailbox_id);
        assert_ne!(a.enc_key, k);
        assert_ne!(GroupKeys::derive(&random_key()).mailbox_id, a.mailbox_id);
    }

    #[test]
    fn item_roundtrip_and_tampering() {
        let key = random_key();
        let id = Uuid::now_v7();
        let blob = encrypt_item(&key, id, &payload());
        assert_eq!(decrypt_item(&key, id, &blob).unwrap(), payload());
        assert!(decrypt_item(&random_key(), id, &blob).is_err());
        assert!(decrypt_item(&key, Uuid::now_v7(), &blob).is_err());
        assert!(decrypt_item(&key, id, "AAAA").is_err());
    }

    #[test]
    fn state_roundtrip() {
        let key = random_key();
        let s = state(&key);
        assert_eq!(decrypt_state(&key, &encrypt_state(&key, &s)).unwrap(), s);
    }

    #[test]
    fn wrapped_state_only_for_recipient() {
        let alice = DeviceKey::generate();
        let bob = DeviceKey::generate();
        let s = state(&random_key());
        let blob = wrap_state(&alice.public_b64(), &s).unwrap();
        assert_eq!(alice.unwrap_state(&blob).unwrap(), s);
        assert!(bob.unwrap_state(&blob).is_err());
        let restored = DeviceKey::from_bytes(alice.to_bytes());
        assert_eq!(restored.unwrap_state(&blob).unwrap(), s);
    }
}
