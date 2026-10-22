//! Commandes appelées par l'interface.

use std::sync::{atomic::Ordering, Arc};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use uuid::Uuid;

use mypress_proto::api::HISTORY_LIMITS;

use crate::{
    clipboard,
    core::{Core, MailboxStatus},
    mailbox, p2p,
    store::Item,
    View,
};

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
pub struct DeviceView {
    id: String,
    name: String,
    current: bool,
    online: bool,
}

#[derive(Serialize)]
pub struct StateView {
    /// "solo", "online", "offline", "paused".
    status: &'static str,
    status_label: String,
    notice: Option<String>,
    device_id: String,
    device_name: String,
    devices: Vec<DeviceView>,
    mailbox: Option<String>,
    mailbox_status: MailboxStatus,
    history_limit: u32,
    history_limits: Vec<u32>,
    capture_paused: bool,
    sync_paused: bool,
    autostart: bool,
}

/// Résumé lisible de l'état de la synchronisation (aussi utilisé par le menu de l'icône).
pub fn status(core: &Core) -> (&'static str, String) {
    let group = core.group();
    let others = group.members.len().saturating_sub(1);
    let online = core.online_members().len();
    let mailbox = core.mailbox_status.lock().unwrap().clone();
    if core.sync_paused() {
        return ("paused", "Synchronisation suspendue".into());
    }
    if others == 0 {
        return ("solo", "Aucun autre appareil associé".into());
    }
    let mut label = match online {
        0 => "Aucun appareil joignable".to_string(),
        1 => "1 appareil connecté".to_string(),
        n => format!("{n} appareils connectés"),
    };
    match &mailbox {
        MailboxStatus::Online => label.push_str(" · boîte aux lettres OK"),
        MailboxStatus::Error(_) => label.push_str(" · boîte aux lettres injoignable"),
        MailboxStatus::Connecting => label.push_str(" · connexion à la boîte aux lettres…"),
        MailboxStatus::Disabled => {}
    }
    let ok = online > 0 || mailbox == MailboxStatus::Online;
    (if ok { "online" } else { "offline" }, label)
}

#[tauri::command]
pub fn get_state(app: AppHandle, core: State<Arc<Core>>) -> StateView {
    let group = core.group();
    let online = core.online_members();
    let (status, status_label) = status(&core);
    StateView {
        status,
        status_label,
        notice: core.notice.lock().unwrap().clone(),
        device_id: core.device_id.clone(),
        device_name: core.device_name.clone(),
        devices: group
            .members
            .iter()
            .map(|m| DeviceView {
                id: m.id.clone(),
                name: m.name.clone(),
                current: m.id == core.device_id,
                online: online.contains(&m.id),
            })
            .collect(),
        mailbox: group.mailbox.clone(),
        mailbox_status: core.mailbox_status.lock().unwrap().clone(),
        history_limit: group.history_limit,
        history_limits: HISTORY_LIMITS.to_vec(),
        capture_paused: core.capture_paused(),
        sync_paused: core.sync_paused(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
    }
}

#[tauri::command]
pub fn search_history(core: State<Arc<Core>>, query: String) -> Res<Vec<Item>> {
    core.with_store(|s| s.search(&query, 200)).map_err(err)
}

/// Place l'élément dans le presse-papiers, masque le panneau et colle dans
/// l'application qui était active. Retourne `false` si le collage automatique
/// n'a pas pu être fait (l'élément reste dans le presse-papiers).
#[tauri::command]
pub async fn paste_item(app: AppHandle, core: State<'_, Arc<Core>>, id: Uuid) -> Res<bool> {
    let item = core.with_store(|s| s.item(id)).ok_or("élément introuvable")?;
    core.write_clipboard(&item.text);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    let hwnd = core.prev_window.load(Ordering::Relaxed);
    Ok(tokio::task::spawn_blocking(move || clipboard::focus_and_paste(hwnd)).await.unwrap_or(false))
}

#[tauri::command]
pub fn copy_item(core: State<Arc<Core>>, id: Uuid) -> Res<()> {
    let item = core.with_store(|s| s.item(id)).ok_or("élément introuvable")?;
    core.write_clipboard(&item.text);
    Ok(())
}

/// Copie un texte sans l'enregistrer dans l'historique (ex. code d'invitation).
#[tauri::command]
pub fn copy_text(core: State<Arc<Core>>, text: String) {
    core.write_clipboard(&text);
}

#[tauri::command]
pub fn delete_item(core: State<Arc<Core>>, id: Uuid) -> Res<()> {
    core.delete(id).map_err(err)
}

#[tauri::command]
pub fn clear_history(core: State<Arc<Core>>) -> Res<()> {
    core.clear().map_err(err)
}

#[tauri::command]
pub fn create_invite(core: State<Arc<Core>>) -> String {
    core.create_invite()
}

#[tauri::command]
pub async fn join_group(core: State<'_, Arc<Core>>, code: String) -> Res<()> {
    let core = core.inner().clone();
    if core.group().members.len() > 1 {
        core.leave().await;
    }
    p2p::join(core, &code).await.map_err(err)
}

#[tauri::command]
pub fn revoke_device(core: State<Arc<Core>>, id: String) -> Res<()> {
    core.revoke(&id).map_err(err)
}

#[tauri::command]
pub async fn leave_group(core: State<'_, Arc<Core>>) -> Res<()> {
    core.leave().await;
    Ok(())
}

#[tauri::command]
pub fn set_mailbox(core: State<Arc<Core>>, url: String) -> Res<()> {
    let url = if url.trim().is_empty() { None } else { Some(mailbox::normalize_url(&url).map_err(err)?) };
    core.update_group(|g| g.mailbox = url);
    Ok(())
}

#[tauri::command]
pub fn set_history_limit(core: State<Arc<Core>>, limit: u32) -> Res<()> {
    if !HISTORY_LIMITS.contains(&limit) {
        return Err("valeur non prise en charge".into());
    }
    core.update_group(|g| g.history_limit = limit);
    Ok(())
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Res<()> {
    let auto = app.autolaunch();
    if enabled { auto.enable() } else { auto.disable() }.map_err(err)
}

#[tauri::command]
pub fn set_view(view: State<View>, name: String) {
    *view.0.lock().unwrap() = name;
}

#[tauri::command]
pub fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}
