use std::time::Duration;

use futures_util::StreamExt;
use mypress_proto::{api::*, crypto};
use mypress_server::{db::Db, router, AppState};
use reqwest::{Client, StatusCode};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

async fn spawn() -> String {
    let state = AppState::new(Db::open_in_memory().unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    format!("http://{addr}")
}

fn record(keys: &crypto::GroupKeys, text: &str, created_at: i64) -> Record {
    let id = Uuid::now_v7();
    let p = ItemPayload { kind: ContentKind::Text, text: text.into(), device_name: "PC A".into(), device_id: "a".into() };
    Record { id, created_at, blob: Some(crypto::encrypt_item(&keys.enc_key, id, &p)) }
}

#[tokio::test]
async fn mailbox_flow() {
    let base = spawn().await;
    let http = Client::new();
    let keys = crypto::GroupKeys::derive(&crypto::random_key());
    let g = format!("{base}/v1/{}", keys.mailbox_id);
    let auth = keys.mailbox_secret.clone();

    // WebSocket ouvert avant l'envoi (crée le groupe au passage).
    let mut req = format!("{}/ws", g.replace("http://", "ws://")).into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {auth}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // Mauvais secret refusé.
    let r = http.get(format!("{g}/records")).bearer_auth("x".repeat(44)).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // Envoi idempotent puis réception.
    let a = record(&keys, "bonjour", 1);
    for _ in 0..2 {
        let r = http.put(format!("{g}/records")).bearer_auth(&auth).json(&PushRecords { records: vec![a.clone()] }).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
    }
    let ev = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let m = ws.next().await.unwrap().unwrap();
            if m.is_text() {
                return serde_json::from_str::<MailboxEvent>(m.to_text().unwrap()).unwrap();
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(ev, MailboxEvent::Changed { .. }));

    let p: PullResponse = http.get(format!("{g}/records?since=0")).bearer_auth(&auth).send().await.unwrap().json().await.unwrap();
    assert_eq!(p.records.len(), 1);
    assert_eq!(p.records[0].record, a);
    let since = p.max_seq;

    // Suppression : remplace le contenu ; un renvoi de l'ancien contenu ne le ressuscite pas.
    let tomb = Record { blob: None, ..a.clone() };
    http.put(format!("{g}/records")).bearer_auth(&auth).json(&PushRecords { records: vec![tomb.clone()] }).send().await.unwrap();
    http.put(format!("{g}/records")).bearer_auth(&auth).json(&PushRecords { records: vec![a.clone()] }).send().await.unwrap();
    let p: PullResponse = http.get(format!("{g}/records?since={since}")).bearer_auth(&auth).send().await.unwrap().json().await.unwrap();
    assert_eq!(p.records.len(), 1);
    assert_eq!(p.records[0].record.blob, None);
    let p: PullResponse = http.get(format!("{g}/records?since=0")).bearer_auth(&auth).send().await.unwrap().json().await.unwrap();
    assert_eq!(p.records.len(), 1, "pas de doublon");

    // Limite globale : au-delà de MAX_LIVE, les plus anciens disparaissent.
    let many: Vec<Record> = (0..1010).map(|n| record(&keys, &format!("e{n}"), 100 + n)).collect();
    http.put(format!("{g}/records")).bearer_auth(&auth).json(&PushRecords { records: many[..1000].to_vec() }).send().await.unwrap();
    http.put(format!("{g}/records")).bearer_auth(&auth).json(&PushRecords { records: many[1000..].to_vec() }).send().await.unwrap();
    let mut live = 0;
    let mut since = 0;
    loop {
        let p: PullResponse = http.get(format!("{g}/records?since={since}")).bearer_auth(&auth).send().await.unwrap().json().await.unwrap();
        live += p.records.iter().filter(|r| r.record.blob.is_some()).count();
        since = p.max_seq;
        if !p.more { break; }
    }
    assert_eq!(live, 1000);

    // État : la version la plus haute l'emporte.
    let r = http.get(format!("{g}/state")).bearer_auth(&auth).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    for (v, b) in [(2, "v2"), (1, "v1")] {
        http.put(format!("{g}/state")).bearer_auth(&auth).json(&StateBlob { version: v, blob: b.into() }).send().await.unwrap();
    }
    let s: StateBlob = http.get(format!("{g}/state")).bearer_auth(&auth).send().await.unwrap().json().await.unwrap();
    assert_eq!((s.version, s.blob.as_str()), (2, "v2"));

    // Renouvellement de clé : le groupe est vidé et figé, seul le message reste.
    let rk = Rekey { wrapped: vec![WrappedKey { recipient: "b".into(), blob: "xyz".into() }] };
    let r = http.put(format!("{g}/rekey")).bearer_auth(&auth).json(&rk).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let got: Rekey = http.get(format!("{g}/rekey")).bearer_auth(&auth).send().await.unwrap().json().await.unwrap();
    assert_eq!(got.wrapped[0].blob, "xyz");
    let r = http.get(format!("{g}/records?since=0")).bearer_auth(&auth).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::GONE);
    let r = http.put(format!("{g}/records")).bearer_auth(&auth).json(&PushRecords { records: vec![record(&keys, "x", 1)] }).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::GONE);
}
