//! Boîte aux lettres optionnelle : stocke les éléments chiffrés d'un groupe d'appareils
//! pour ceux qui ne sont pas allumés en même temps. Aucun compte : l'accès se fait avec
//! un secret dérivé de la clé du groupe, et le contenu reste illisible pour le serveur.

pub mod db;
mod routes;

use std::{collections::HashMap, sync::Arc};

use tokio::sync::{broadcast, Mutex};

pub use routes::router;

pub struct AppState {
    pub db: Mutex<db::Db>,
    hubs: std::sync::Mutex<HashMap<String, broadcast::Sender<String>>>,
}

impl AppState {
    pub fn new(db: db::Db) -> Arc<Self> {
        Arc::new(Self { db: Mutex::new(db), hubs: Default::default() })
    }

    pub fn subscribe(&self, group: &str) -> broadcast::Receiver<String> {
        let mut hubs = self.hubs.lock().unwrap();
        hubs.entry(group.to_string()).or_insert_with(|| broadcast::channel(64).0).subscribe()
    }

    pub fn notify(&self, group: &str, event: &mypress_proto::api::MailboxEvent) {
        let mut hubs = self.hubs.lock().unwrap();
        if let Some(tx) = hubs.get(group) {
            if tx.send(serde_json::to_string(event).unwrap()).is_err() {
                hubs.remove(group);
            }
        }
    }
}
