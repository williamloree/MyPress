//! Second appareil en ligne de commande, sur le vrai réseau iroh (n0), pour tester l'application.
//!
//! cargo run -p mypress --example peer -- <code d'invitation> "<texte à copier>"

use std::{sync::Arc, time::Duration};

use mypress_lib::{
    core::{Core, Host, Secrets},
    p2p,
    store::Store,
};

struct PrintHost;

impl Host for PrintHost {
    fn history_changed(&self) {}
    fn status_changed(&self) {}
    fn write_clipboard(&self, text: &str) -> anyhow::Result<()> {
        println!("[presse-papiers] {text:?}");
        Ok(())
    }
    fn write_image(&self, png_b64: &str) -> anyhow::Result<()> {
        println!("[presse-papiers] image de {} octets (base64)", png_b64.len());
        Ok(())
    }
}

struct NoSecrets;

impl Secrets for NoSecrets {
    fn load(&self) -> Option<String> {
        None
    }
    fn save(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [code, text] = args.as_slice() else { anyhow::bail!("usage : peer <code> <texte>") };
    let core = Arc::new(Core::new(Arc::new(PrintHost), &NoSecrets, Store::open_in_memory()?, "Pair de test".into())?);
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(core.iroh_key.clone())
        .alpns(vec![p2p::ALPN.to_vec()])
        .bind()
        .await?;
    p2p::start(core.clone(), endpoint, None);

    p2p::join(core.clone(), code).await?;
    println!("associé : {} membres", core.group().members.len());
    tokio::time::sleep(Duration::from_secs(2)).await;
    core.on_clipboard_text(text.clone());
    tokio::time::sleep(Duration::from_secs(3)).await;
    for item in core.with_store(|s| s.search("", 100))? {
        println!("[historique] {} : {:?}", item.device_name, item.text);
    }
    // Quitte le groupe pour ne pas laisser d'appareil fantôme dans l'application.
    core.leave().await;
    Ok(())
}
