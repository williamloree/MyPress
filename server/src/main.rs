use std::path::PathBuf;

use mypress_server::{db::Db, router, AppState};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let bind = std::env::var("MYPRESS_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let db_path = PathBuf::from(std::env::var("MYPRESS_DB").unwrap_or_else(|_| "mypress.db".into()));

    let state = AppState::new(Db::open(&db_path)?);
    // Les journaux n'enregistrent que la méthode, le chemin et le statut : jamais de contenu ni de secret.
    let app = router(state).layer(tower_http::trace::TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("boîte aux lettres à l'écoute sur {bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
