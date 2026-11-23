//! Chiffrement de bout en bout.
//!
//! Chaque groupe d'appareils partage une clé aléatoire de 32 octets. Par HKDF on en dérive :
//! - la clé de chiffrement des éléments (XChaCha20-Poly1305) ;
//! - l'identifiant et le secret d'accès de la boîte aux lettres, qui ne permettent pas de déchiffrer.
//!
//! Chaque appareil possède aussi une paire X25519 : lors d'une révocation, la nouvelle clé
//! de groupe est chiffrée séparément pour chaque appareil restant.
//!
//! Le coffre de mots de passe a ses propres clés aléatoires (voir [`crate::api::VaultKeys`]),
//! éventuellement scellées par une phrase de passe (Argon2id). Ses révisions restent
//! chiffrées partout : sur le disque, en P2P et dans la boîte aux lettres.

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

use crate::api::{GroupState, ItemPayload, VaultPayload};

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
    /// Espace séparé de la boîte aux lettres pour le coffre : ses révisions ne
    /// comptent pas dans la limite de l'historique.
    pub vault_mailbox_id: String,
    pub vault_mailbox_secret: String,
}

impl GroupKeys {
    pub fn derive(group_key: &[u8; 32]) -> Self {
        let hk = Hkdf::<Sha256>::new(None, group_key);
        let expand = |info: &[u8]| {
            let mut out = [0u8; 32];
            hk.expand(info, &mut out).expect("longueur HKDF valide");
            out
        };
        let hex = |b: [u8; 32]| b.iter().map(|b| format!("{b:02x}")).collect();
        Self {
            enc_key: expand(b"mypress enc v1"),
            mailbox_id: hex(expand(b"mypress mailbox-id v1")),
            mailbox_secret: b64(&expand(b"mypress mailbox-auth v1")),
            vault_mailbox_id: hex(expand(b"mypress vault mailbox-id v1")),
            vault_mailbox_secret: b64(&expand(b"mypress vault mailbox-auth v1")),
        }
    }

    pub fn from_state(state: &GroupState) -> Result<Self, CryptoError> {
        Ok(Self::derive(&key_from_b64(&state.key)?))
    }
}

/// Chiffre avec une clé symétrique ; `aad` lie le résultat à son usage.
pub fn seal(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> String {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, Payload { msg: plain, aad }).expect("chiffrement");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    B64.encode(out)
}

pub fn open(key: &[u8; 32], aad: &[u8], blob: &str) -> Result<Vec<u8>, CryptoError> {
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

fn vault_aad(id: Uuid) -> Vec<u8> {
    [b"mypress vault v1 ".as_slice(), id.as_bytes()].concat()
}

/// Chiffre une révision du coffre avec la clé courante du coffre.
pub fn encrypt_vault(key: &[u8; 32], id: Uuid, payload: &VaultPayload) -> String {
    seal(key, &vault_aad(id), &serde_json::to_vec(payload).expect("sérialisable"))
}

/// Essaie les clés du coffre, de la plus récente à la plus ancienne.
pub fn decrypt_vault(keys: &[[u8; 32]], id: Uuid, blob: &str) -> Result<VaultPayload, CryptoError> {
    let aad = vault_aad(id);
    let plain = keys.iter().rev().find_map(|k| open(k, &aad, blob).ok()).ok_or(CryptoError::Invalid)?;
    serde_json::from_slice(&plain).map_err(|_| CryptoError::Invalid)
}

/// Clé dérivée d'une phrase de passe (Argon2id, 64 Mio, 3 passes) : quelques
/// centaines de millisecondes, pour freiner les essais en masse.
pub fn passphrase_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], CryptoError> {
    let params = argon2::Params::new(64 * 1024, 3, 1, Some(32)).map_err(|_| CryptoError::Key)?;
    let mut out = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|_| CryptoError::Key)?;
    Ok(out)
}

pub fn random_salt() -> String {
    b64(&random_key()[..16])
}

pub fn salt_from_b64(s: &str) -> Result<Vec<u8>, CryptoError> {
    B64.decode(s).map_err(|_| CryptoError::Key)
}

/// Scelle les clés du coffre avec la clé dérivée de la phrase de passe.
pub fn seal_vault_keys(derived: &[u8; 32], keys: &[[u8; 32]]) -> String {
    let list: Vec<String> = keys.iter().map(|k| b64(k)).collect();
    seal(derived, b"mypress vault-keys v1", &serde_json::to_vec(&list).expect("sérialisable"))
}

/// Erreur si la phrase de passe est fausse.
pub fn open_vault_keys(derived: &[u8; 32], blob: &str) -> Result<Vec<[u8; 32]>, CryptoError> {
    let list: Vec<String> =
        serde_json::from_slice(&open(derived, b"mypress vault-keys v1", blob)?).map_err(|_| CryptoError::Invalid)?;
    list.iter().map(|k| key_from_b64(k)).collect()
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
        GroupState { epoch: 1, version: 3, key: b64(key), members: vec![], history_limit: 100, mailbox: None, vault: None }
    }

    #[test]
    fn vault_revision_with_rotated_keys() {
        let (old, new) = (random_key(), random_key());
        let id = Uuid::now_v7();
        let p = VaultPayload {
            entry: Uuid::now_v7(),
            updated_at: 5,
            device_name: "PC".into(),
            fields: crate::api::VaultFields { name: "Banque".into(), password: "s3cret!".into(), ..Default::default() },
        };
        let blob = encrypt_vault(&old, id, &p);
        assert_eq!(decrypt_vault(&[old, new], id, &blob).unwrap(), p, "ancienne clé encore lisible");
        assert!(decrypt_vault(&[new], id, &blob).is_err());
        assert!(decrypt_vault(&[old], Uuid::now_v7(), &blob).is_err(), "révision non déplaçable");
        // Une clé d'historique ne lit pas le coffre, même avec le même identifiant.
        assert!(decrypt_item(&old, id, &blob).is_err());
    }

    #[test]
    fn vault_keys_sealed_by_passphrase() {
        let salt = salt_from_b64(&random_salt()).unwrap();
        let keys = [random_key(), random_key()];
        let derived = passphrase_key("cheval agrafe correct", &salt).unwrap();
        let blob = seal_vault_keys(&derived, &keys);
        assert_eq!(passphrase_key("cheval agrafe correct", &salt).unwrap(), derived, "dérivation stable");
        assert_eq!(open_vault_keys(&derived, &blob).unwrap(), keys);
        let wrong = passphrase_key("cheval agrafe incorrect", &salt).unwrap();
        assert!(open_vault_keys(&wrong, &blob).is_err());
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
