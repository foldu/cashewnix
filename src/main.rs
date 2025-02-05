use config::Config;
use eyre::{Context, ContextCompat};
use tokio::{net::TcpListener, signal::unix::SignalKind};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::{binary_cache_proxy::Server, signing::parse_ssh_private_key};

mod binary_cache_proxy;
mod config;
mod discover;
mod signing;
mod util;

async fn run(config: Config) -> Result<(), eyre::Error> {
    let token = CancellationToken::new();
    let mut set = tokio::task::JoinSet::new();
    let shutdown = {
        let token = token.clone();
        let signal =
            |sig| tokio::signal::unix::signal(sig).context("Failed installing signal handler");
        let mut int = signal(SignalKind::interrupt())?;
        let mut term = signal(SignalKind::terminate())?;
        async move {
            tokio::select! {
                _ = term.recv() => (),
                _ = int.recv() => (),
                _ = token.cancelled() => (),
            }
        }
    };

    let mut keystore = signing::KeyStore::default();
    for key in &config.public_keys {
        keystore.add_public_key(key.clone());
    }
    if let Ok(key) = std::env::var("NIX_STORE_PRIVATE_KEY") {
        let key = parse_ssh_private_key(&key)?;
        keystore.init_private_key(key);
    }

    if config
        .local_binary_caches
        .as_ref()
        .and_then(|entry| entry.local_cache.as_ref())
        .is_some()
        && !keystore.has_private_key()
    {
        eyre::bail!("Refusing to operate without private key while advertising a store");
    }

    let port = config.port;
    let server = Server::new(&mut set, config, keystore, token.clone()).await?;
    let router = binary_cache_proxy::routes().with_state(server);
    let listener = TcpListener::bind(("127.0.0.1", port.get())).await?;

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await?;

    tracing::info!("Shutting down");

    token.cancel();

    while let Some(res) = set.join_next().await {
        // FIXME:
        match res {
            Ok(Err(e)) => {
                Err(e).context("Failed joining task")?;
            }
            Err(e) => {
                Err(e).context("Task crashed")?;
            }
            _ => (),
        }
    }

    Ok(())
}

fn main() -> Result<(), eyre::Error> {
    if let Err(std::env::VarError::NotPresent) = std::env::var("RUST_LOG") {
        std::env::set_var("RUST_LOG", "info")
    }
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let config_path = std::env::var_os("CONFIG_PATH")
        .context("Missing CONFIG_PATH env variable, no idea how to find config")?;
    let config = Config::load(config_path)?;
    rt.block_on(run(config))?;
    Ok(())
}
