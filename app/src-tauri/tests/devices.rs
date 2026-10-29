//! Plusieurs appareils dans un même processus : P2P iroh réel (en local, sans relais)
//! et boîte aux lettres réelle.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh::{address_lookup::MemoryLookup, endpoint::presets, Endpoint, RelayMode};
use mypress_lib::{
    core::{Core, Host, MailboxStatus, Secrets},
    mailbox, p2p,
    share::{self, Share},
    store::Store,
};

#[derive(Default)]
struct TestHost {
    clipboard: Mutex<Vec<String>>,
}

impl Host for TestHost {
    fn history_changed(&self) {}
    fn status_changed(&self) {}
    fn write_clipboard(&self, text: &str) -> anyhow::Result<()> {
        self.clipboard.lock().unwrap().push(text.to_string());
        Ok(())
    }
    fn write_image(&self, png_b64: &str) -> anyhow::Result<()> {
        self.clipboard.lock().unwrap().push(png_b64.to_string());
        Ok(())
    }
}

#[derive(Default)]
struct MemSecrets(Mutex<Option<String>>);

impl Secrets for MemSecrets {
    fn load(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, v: &str) -> anyhow::Result<()> {
        *self.0.lock().unwrap() = Some(v.to_string());
        Ok(())
    }
}

struct Dev {
    core: Arc<Core>,
    host: Arc<TestHost>,
    share: Arc<Share>,
}

impl Dev {
    async fn new(name: &str, lookup: &MemoryLookup) -> Dev {
        let host = Arc::new(TestHost::default());
        let core = Arc::new(Core::new(host.clone(), &MemSecrets::default(), Store::open_in_memory().unwrap(), name.into()).unwrap());
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(core.iroh_key.clone())
            .alpns(vec![p2p::ALPN.to_vec(), share::ALPN.to_vec()])
            .relay_mode(RelayMode::Disabled)
            .address_lookup(lookup.clone())
            .bind_addr("127.0.0.1:0")
            .unwrap()
            .bind()
            .await
            .unwrap();
        lookup.add_endpoint_info(endpoint.addr());
        let dir = std::env::temp_dir().join(format!("mypress-test-{}", uuid::Uuid::now_v7()));
        let share = Arc::new(Share::new(core.clone(), dir, || {}));
        p2p::start(core.clone(), endpoint, Some(share.clone()));
        tokio::spawn(mailbox::run(core.clone()));
        Dev { core, host, share }
    }

    fn copy(&self, text: &str) {
        self.core.on_clipboard_text(text.to_string());
    }

    fn texts(&self) -> Vec<String> {
        self.core.with_store(|s| s.search("", 1000)).unwrap().into_iter().map(|i| i.text).collect()
    }

    fn has(&self, text: &str) -> bool {
        self.texts().iter().any(|t| t == text)
    }
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..300 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("délai dépassé : {what}");
}

async fn mailbox_server() -> String {
    let state = mypress_server::AppState::new(mypress_server::db::Db::open_in_memory().unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, mypress_server::router(state)).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_of_devices() {
    let _ = tracing_subscriber::fmt().with_env_filter("mypress_lib=debug").with_test_writer().try_init();
    let lookup = MemoryLookup::new();
    let a = Dev::new("PC A", &lookup).await;
    let b = Dev::new("PC B", &lookup).await;
    let c = Dev::new("PC C", &lookup).await;
    for d in [&a, &b, &c] {
        eprintln!("ID {} = {}", &d.core.device_id[..6], d.core.device_name);
    }

    // --- Appairage : les historiques existants fusionnent.
    a.copy("avant appairage A");
    b.copy("avant appairage B");
    let code = a.core.create_invite();
    p2p::join(b.core.clone(), &code).await.unwrap();
    until("B dans le groupe de A", || a.core.group().members.len() == 2).await;
    until("fusion des historiques", || a.has("avant appairage B") && b.has("avant appairage A")).await;

    // Code à usage unique.
    let err = p2p::join(c.core.clone(), &code).await.unwrap_err();
    assert!(err.to_string().contains("invalide"), "{err}");

    // --- Copie sur B : visible sur A et placée dans son presse-papiers.
    b.copy("ligne 1\nligne 2 — é");
    until("copie B → A", || a.has("ligne 1\nligne 2 — é")).await;
    until("presse-papiers de A", || a.host.clipboard.lock().unwrap().last().map(String::as_str) == Some("ligne 1\nligne 2 — é")).await;
    assert!(!b.texts().iter().filter(|t| t.starts_with("ligne 1")).nth(1).is_some(), "pas de doublon par écho");

    // --- Image copiée sur B : collée telle quelle sur A, aperçu dans l'historique.
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(40, 30, image::Rgba([200, 30, 30, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();
    let png = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png.into_inner());
    b.core.on_clipboard_image(png.clone());
    until("image B → A", || a.host.clipboard.lock().unwrap().last() == Some(&png)).await;
    let latest = a.core.with_store(|s| s.search("", 1)).unwrap().remove(0);
    assert_eq!(latest.kind, "image");
    assert!(!latest.text.is_empty() && latest.text != png, "aperçu à la place de l'image");
    assert!(a.core.with_store(|s| s.search("a", 100)).unwrap().iter().all(|i| i.kind != "image"), "images hors recherche");

    // --- C rejoint via B ; tout le monde se connecte à tout le monde.
    p2p::join(c.core.clone(), &b.core.create_invite()).await.unwrap();
    until("3 membres partout", || [&a, &b, &c].iter().all(|d| d.core.group().members.len() == 3)).await;
    until("C a l'historique", || c.has("avant appairage A") && c.has("ligne 1\nligne 2 — é")).await;
    until("A connecté à C", || a.core.online_members().len() == 2).await;

    // --- Suppression et paramètres propagés.
    let id = a.core.with_store(|s| s.search("avant appairage B", 1)).unwrap()[0].id;
    a.core.delete(id).unwrap();
    until("suppression propagée", || !b.has("avant appairage B") && !c.has("avant appairage B")).await;
    a.core.update_group(|g| g.history_limit = 50);
    until("limite propagée", || c.core.history_limit() == 50).await;

    // --- B hors ligne, puis reprise : rattrapage en P2P.
    b.core.set_sync_paused(true);
    a.copy("pendant la pause de B");
    until("C reçoit", || c.has("pendant la pause de B")).await;
    assert!(!b.has("pendant la pause de B"));
    b.core.set_sync_paused(false);
    until("B rattrape", || b.has("pendant la pause de B")).await;

    // --- Boîte aux lettres : A et C ne sont jamais allumés en même temps.
    let url = mailbox_server().await;
    a.core.update_group(|g| g.mailbox = Some(url.clone()));
    until("boîte configurée partout", || c.core.group().mailbox.as_deref() == Some(url.as_str())).await;
    until("boîtes en ligne", || {
        [&a, &b, &c].iter().all(|d| *d.core.mailbox_status.lock().unwrap() == MailboxStatus::Online)
    })
    .await;
    c.core.set_sync_paused(true);
    b.core.set_sync_paused(true);
    a.copy("déposé dans la boîte");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    a.core.set_sync_paused(true);
    c.core.set_sync_paused(false);
    until("C reçoit via la boîte", || c.has("déposé dans la boîte")).await;
    assert!(a.core.online_members().is_empty() && c.core.online_members().is_empty(), "pas de P2P pendant ce test");

    // --- Révocation de C par A pendant que B est hors ligne.
    a.core.set_sync_paused(false);
    until("A et C reconnectés", || a.core.online_members().len() == 1).await;
    let old_epoch = a.core.group().epoch;
    let c_id = c.core.device_id.clone();
    a.core.revoke(&c_id).unwrap();
    until("C sait qu'il est retiré", || c.core.group().members.len() == 1 && c.core.notice.lock().unwrap().is_some()).await;
    // A dépose la nouvelle clé dans l'ancienne boîte, puis s'éteint.
    until("renouvellement déposé", || a.core.with_store(|s| s.get("pending_rekey")).is_none()).await;
    a.core.set_sync_paused(true);
    // B revient : seule la boîte aux lettres peut lui transmettre la nouvelle clé.
    b.core.set_sync_paused(false);
    until("B a la nouvelle clé", || b.core.group().epoch == old_epoch + 1).await;
    assert!(!b.core.group().is_member(&c_id));

    // Après révocation : A et B échangent, C ne reçoit plus rien.
    a.core.set_sync_paused(false);
    until("A et B reconnectés", || a.core.online_members().len() == 1).await;
    a.copy("après révocation");
    until("B reçoit", || b.has("après révocation")).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!c.has("après révocation"));
    assert!(a.core.online_members().iter().all(|m| *m != c_id));

    // --- Révocation avec tout le monde en ligne : B, connecté à C, ne doit pas être entraîné.
    p2p::join(c.core.clone(), &a.core.create_invite()).await.unwrap();
    until("C de retour", || [&a, &b, &c].iter().all(|d| d.core.group().members.len() == 3)).await;
    until("B connecté à C", || b.core.online_members().contains(&c_id)).await;
    a.core.revoke(&c_id).unwrap();
    until("B suit la révocation", || b.core.group().epoch == a.core.group().epoch && !b.core.group().is_member(&c_id)).await;
    until("C retiré", || c.core.group().members.len() == 1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(b.core.group().members.len(), 2, "B reste dans le groupe");
    assert!(b.core.notice.lock().unwrap().is_none());
    b.copy("fin");
    until("A reçoit encore de B", || a.has("fin")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn share_group_and_files() {
    let lookup = MemoryLookup::new();
    let a = Dev::new("PC Alice", &lookup).await;
    let b = Dev::new("PC Bob", &lookup).await;
    let c = Dev::new("PC Chloé", &lookup).await;

    // Trois personnes, chacune avec ses propres appareils : aucun n'est associé.
    let g = a.share.create_group("Équipe").unwrap();
    b.share.join(&a.share.invite(&g.id).unwrap()).await.unwrap();
    c.share.join(&a.share.invite(&g.id).unwrap()).await.unwrap();
    until("3 membres partout", || [&a, &b, &c].iter().all(|d| d.share.groups().first().is_some_and(|g| g.members.len() == 3))).await;
    assert!(a.core.group().members.len() == 1, "le presse-papiers reste séparé");

    // Fichier envoyé au groupe : reçu à l'identique par les deux autres.
    let src = std::env::temp_dir().join(format!("mypress-src-{}.bin", uuid::Uuid::now_v7()));
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(&src, &data).unwrap();
    assert_eq!(a.share.send_files(&g.id, std::slice::from_ref(&src)).await.unwrap(), (2, 2));
    for d in [&b, &c] {
        let t = d.share.transfers().remove(0);
        assert!(t.incoming && t.peer == "PC Alice" && t.group == "Équipe");
        assert_eq!(std::fs::read(&t.path).unwrap(), data);
    }
    // Même nom une seconde fois : pas d'écrasement.
    a.share.send_files(&g.id, std::slice::from_ref(&src)).await.unwrap();
    assert_ne!(b.share.transfers()[0].path, b.share.transfers()[1].path);

    // Bob quitte : il ne reçoit plus rien.
    b.share.leave(&g.id);
    until("Bob retiré", || a.share.groups()[0].members.len() == 2 && c.share.groups()[0].members.len() == 2).await;
    assert!(b.share.groups().is_empty());
    assert_eq!(a.share.send_files(&g.id, std::slice::from_ref(&src)).await.unwrap(), (1, 1));

    // « Mes appareils » : aucun autre appareil associé.
    assert!(a.share.send_files(share::ME, std::slice::from_ref(&src)).await.is_err());
    // Bob n'a plus le groupe.
    assert!(b.share.send_files(&g.id, &[src]).await.is_err());
}
