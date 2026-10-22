use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        FromRequestParts, Path, Query, State, WebSocketUpgrade,
    },
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use mypress_proto::api::*;

use crate::{db::GroupAccess, AppState};

type St = State<Arc<AppState>>;

pub struct AppError(StatusCode, &'static str);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!("base de données : {e}");
        AppError(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne")
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/{group}/records", get(pull).put(push))
        .route("/v1/{group}/state", get(get_state).put(put_state))
        .route("/v1/{group}/rekey", get(get_rekey).put(put_rekey))
        .route("/v1/{group}/ws", get(ws))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .with_state(state)
}

/// Groupe authentifié. `frozen` : le groupe a été remplacé par un renouvellement de clé.
pub struct Group {
    id: String,
    frozen: bool,
}

impl FromRequestParts<Arc<AppState>> for Group {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, AppError> {
        const DENIED: AppError = AppError(StatusCode::UNAUTHORIZED, "accès refusé");
        let Path(params) = Path::<std::collections::HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError(StatusCode::BAD_REQUEST, "groupe invalide"))?;
        let id = params.get("group").cloned().unwrap_or_default();
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AppError(StatusCode::BAD_REQUEST, "groupe invalide"));
        }
        let secret = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|s| s.len() >= 32)
            .ok_or(DENIED)?;
        let hash = Sha256::digest(secret.as_bytes());
        match state.db.lock().await.access(&id, &hash)? {
            GroupAccess::Ok => Ok(Group { id, frozen: false }),
            GroupAccess::Frozen => Ok(Group { id, frozen: true }),
            GroupAccess::Denied => Err(DENIED),
        }
    }
}

const GONE: AppError = AppError(StatusCode::GONE, "groupe remplacé par une nouvelle clé");

impl Group {
    fn active(&self) -> Result<(), AppError> {
        if self.frozen { Err(GONE) } else { Ok(()) }
    }
}

#[derive(Deserialize)]
struct PullQuery {
    #[serde(default)]
    since: i64,
}

async fn pull(State(state): St, g: Group, Query(q): Query<PullQuery>) -> Result<Json<PullResponse>, AppError> {
    g.active()?;
    let (records, more) = state.db.lock().await.pull(&g.id, q.since)?;
    let max_seq = records.last().map(|r| r.seq).unwrap_or(q.since);
    Ok(Json(PullResponse { records, max_seq, more }))
}

async fn push(State(state): St, g: Group, Json(req): Json<PushRecords>) -> Result<StatusCode, AppError> {
    g.active()?;
    if req.records.len() > 1000 {
        return Err(AppError(StatusCode::BAD_REQUEST, "trop d'éléments dans une requête"));
    }
    if req.records.iter().any(|r| r.blob.as_ref().is_some_and(|b| b.len() > MAX_BLOB_LEN)) {
        return Err(AppError(StatusCode::PAYLOAD_TOO_LARGE, "élément trop volumineux"));
    }
    let seq = state.db.lock().await.push(&g.id, &req.records)?;
    state.notify(&g.id, &MailboxEvent::Changed { seq });
    Ok(StatusCode::NO_CONTENT)
}

async fn get_state(State(state): St, g: Group) -> Result<Response, AppError> {
    g.active()?;
    Ok(match state.db.lock().await.state(&g.id)? {
        Some(s) => Json(s).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    })
}

async fn put_state(State(state): St, g: Group, Json(s): Json<StateBlob>) -> Result<StatusCode, AppError> {
    g.active()?;
    if s.blob.len() > MAX_BLOB_LEN {
        return Err(AppError(StatusCode::PAYLOAD_TOO_LARGE, "état trop volumineux"));
    }
    if state.db.lock().await.put_state(&g.id, &s)? {
        state.notify(&g.id, &MailboxEvent::State { version: s.version });
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn get_rekey(State(state): St, g: Group) -> Result<Response, AppError> {
    Ok(match state.db.lock().await.get_rekey(&g.id)? {
        Some(body) => ([(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    })
}

async fn put_rekey(State(state): St, g: Group, Json(r): Json<Rekey>) -> Result<StatusCode, AppError> {
    let body = serde_json::to_string(&r).unwrap();
    if body.len() > MAX_BLOB_LEN {
        return Err(AppError(StatusCode::PAYLOAD_TOO_LARGE, "trop volumineux"));
    }
    state.db.lock().await.rekey(&g.id, &body)?;
    state.notify(&g.id, &MailboxEvent::Rekey);
    Ok(StatusCode::NO_CONTENT)
}

async fn ws(State(state): St, g: Group, upgrade: WebSocketUpgrade) -> Response {
    let rx = state.subscribe(&g.id);
    upgrade.on_upgrade(move |socket| ws_loop(socket, rx))
}

async fn ws_loop(mut socket: WebSocket, mut rx: tokio::sync::broadcast::Receiver<String>) {
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let json = match msg {
                    Ok(j) => j,
                    // Notifications perdues : le client resynchronise depuis son dernier numéro.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) =>
                        serde_json::to_string(&MailboxEvent::Changed { seq: i64::MAX }).unwrap(),
                    Err(_) => break,
                };
                if socket.send(Message::Text(json.into())).await.is_err() { break; }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
}
