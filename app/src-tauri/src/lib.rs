mod clipboard;
mod commands;
pub mod core;
pub mod mailbox;
pub mod p2p;
pub mod share;
pub mod store;

use std::sync::{atomic::Ordering, Arc, Mutex};

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Listener, Manager, WindowEvent,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::core::{Core, Host, Secrets};

pub const HISTORY_SHORTCUT: &str = "Ctrl+Alt+V";

/// Vue affichée dans la fenêtre : "history" (panneau rapide) ou "settings".
pub struct View(pub Mutex<String>);

struct TauriHost(AppHandle);

impl Host for TauriHost {
    fn history_changed(&self) {
        let _ = self.0.emit("history-changed", ());
    }
    fn status_changed(&self) {
        let _ = self.0.emit("status-changed", ());
    }
    fn write_clipboard(&self, text: &str) -> anyhow::Result<()> {
        clipboard::write_text(text)
    }
    fn write_image(&self, png_b64: &str) -> anyhow::Result<()> {
        clipboard::write_image(png_b64)
    }
}

/// Clés de l'appareil dans le coffre du système (Gestionnaire d'identification Windows,
/// Trousseau macOS, Secret Service sous Linux). Sans coffre disponible (certains bureaux
/// Linux), repli sur un fichier lisible par l'utilisateur seul.
struct Keyring {
    fallback: std::path::PathBuf,
}

impl Keyring {
    fn entry() -> keyring::Result<keyring::Entry> {
        keyring::Entry::new("MyPress", "device")
    }
}

impl Secrets for Keyring {
    fn load(&self) -> Option<String> {
        Self::entry()
            .and_then(|e| e.get_password())
            .ok()
            .or_else(|| std::fs::read_to_string(&self.fallback).ok())
    }
    fn save(&self, value: &str) -> anyhow::Result<()> {
        match Self::entry().and_then(|e| e.set_password(value)) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("coffre du système indisponible ({e}), clés enregistrées dans un fichier protégé");
                std::fs::write(&self.fallback, value)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&self.fallback, std::fs::Permissions::from_mode(0o600))?;
                }
                Ok(())
            }
        }
    }
}

fn show_view(app: &AppHandle, view: &str) {
    let Some(w) = app.get_webview_window("main") else { return };
    if view == "history" {
        let core = app.state::<Arc<Core>>();
        core.prev_window.store(clipboard::foreground_window(), Ordering::Relaxed);
    }
    *app.state::<View>().0.lock().unwrap() = view.to_string();
    let _ = app.emit("show-view", view);
    let _ = w.center();
    let _ = w.show();
    let _ = w.set_focus();
}

fn toggle_history(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else { return };
    if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
        let _ = w.hide();
    } else {
        show_view(app, "history");
    }
}

fn build_tray(app: &tauri::App, core: &Arc<Core>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", format!("Ouvrir l'historique ({HISTORY_SHORTCUT})"), true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", commands::status(core).1, false, None::<&str>)?;
    let capture = CheckMenuItem::with_id(app, "capture", "Mode privé (ne rien enregistrer)", true, core.capture_paused(), None::<&str>)?;
    let sync = CheckMenuItem::with_id(app, "sync", "Suspendre la synchronisation", true, core.sync_paused(), None::<&str>)?;
    let share = MenuItem::with_id(app, "share", "Envoyer des fichiers…", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Appareils et paramètres…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(app, &[&open, &sep()?, &status, &capture, &sync, &sep()?, &share, &settings, &quit])?;

    let handle = app.handle().clone();
    app.listen("status-changed", move |_| {
        let core = handle.state::<Arc<Core>>();
        let _ = status.set_text(commands::status(&core).1);
    });

    TrayIconBuilder::with_id("tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("MyPress — presse-papiers universel")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| {
            let core = app.state::<Arc<Core>>();
            match event.id().as_ref() {
                "open" => show_view(app, "history"),
                "settings" => show_view(app, "settings"),
                "share" => show_view(app, "share"),
                "capture" => core.set_capture_paused(capture.is_checked().unwrap_or(false)),
                "sync" => core.set_sync_paused(sync.is_checked().unwrap_or(false)),
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_view(tray.app_handle(), "history");
            }
        })
        .build(app)?;
    Ok(())
}

/// Vérifie les releases GitHub au démarrage puis toutes les 6 heures ; installe et relance.
async fn auto_update(app: AppHandle) {
    use tauri_plugin_updater::UpdaterExt;
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    loop {
        let update = match app.updater() {
            Ok(u) => u.check().await,
            Err(e) => Err(e),
        };
        match update {
            Ok(Some(update)) => {
                tracing::info!("mise à jour {} disponible, installation", update.version);
                match update.download_and_install(|_, _| {}, || {}).await {
                    // Sous Windows l'installateur ferme déjà l'application.
                    Ok(()) => app.restart(),
                    Err(e) => tracing::warn!("mise à jour échouée : {e}"),
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("vérification des mises à jour impossible : {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,iroh=warn,noq=warn".into()),
        )
        .init();

    let shortcut = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyV);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show_view(app, "settings")))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, s, event| {
                    if s == &shortcut && event.state() == ShortcutState::Pressed {
                        toggle_history(app);
                    }
                })
                .build(),
        )
        .manage(View(Mutex::new("history".into())))
        .setup(move |app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let store = store::Store::open(&dir.join("history-v2.db"))?;
            let device_name = gethostname::gethostname()
                .into_string()
                .ok()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Ordinateur".into());
            let host = Arc::new(TauriHost(app.handle().clone()));
            let secrets = Keyring { fallback: dir.join("device.key") };
            let core = Arc::new(Core::new(host, &secrets, store, device_name)?);
            app.manage(core.clone());
            let received = app.path().download_dir().map(|d| d.join("MyPress")).unwrap_or_else(|_| dir.join("recus"));
            let handle = app.handle().clone();
            let share = Arc::new(share::Share::new(core.clone(), received, move || {
                let _ = handle.emit("shares-changed", ());
            }));
            app.manage(share.clone());

            build_tray(app, &core)?;
            tauri::async_runtime::spawn(auto_update(app.handle().clone()));

            if let Err(e) = app.global_shortcut().register(shortcut) {
                tracing::error!("raccourci {HISTORY_SHORTCUT} indisponible : {e}");
            }

            let c = core.clone();
            clipboard::watch(move |clip| match clip {
                clipboard::Clip::Text(text) => c.on_clipboard_text(text),
                clipboard::Clip::Image(png) => c.on_clipboard_image(png),
            });

            let c = core.clone();
            tauri::async_runtime::spawn(async move {
                // Connexion directe si possible, relais public n0 sinon ; adresse publiée via DNS n0.
                match iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                    .secret_key(c.iroh_key.clone())
                    .alpns(vec![p2p::ALPN.to_vec(), share::ALPN.to_vec()])
                    .bind()
                    .await
                {
                    Ok(endpoint) => p2p::start(c.clone(), endpoint, Some(share)),
                    Err(e) => tracing::error!("réseau P2P indisponible : {e}"),
                }
                mailbox::run(c).await;
            });

            // Le panneau se ferme dès qu'il perd le focus ; la croix ne fait que masquer.
            let w = app.get_webview_window("main").unwrap();
            let handle = app.handle().clone();
            w.on_window_event(move |event| match event {
                WindowEvent::Focused(false) => {
                    if *handle.state::<View>().0.lock().unwrap() == "history" {
                        if let Some(w) = handle.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    }
                }
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    if let Some(w) = handle.get_webview_window("main") {
                        let _ = w.hide();
                    }
                }
                _ => {}
            });

            // Premier lancement : afficher les appareils pour proposer l'association.
            if core.with_store(|s| s.get("onboarded")).is_none() {
                core.with_store(|s| s.set("onboarded", "1"));
                show_view(app.handle(), "settings");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::search_history,
            commands::paste_item,
            commands::copy_item,
            commands::copy_text,
            commands::delete_item,
            commands::clear_history,
            commands::create_invite,
            commands::join_group,
            commands::revoke_device,
            commands::leave_group,
            commands::set_mailbox,
            commands::set_history_limit,
            commands::set_autostart,
            commands::set_view,
            commands::hide_window,
            commands::share_state,
            commands::create_share_group,
            commands::share_invite,
            commands::join_share_group,
            commands::leave_share_group,
            commands::send_files,
            commands::reveal_file,
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de l'application");
}
