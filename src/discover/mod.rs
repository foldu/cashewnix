use crate::discover::proto::LocalCache;
use ahash::HashMap;
use std::{net::IpAddr, sync::Arc, time::Duration};
use url::Url;

use tokio::{net::UdpSocket, sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;

use crate::{config::Config, discover::proto::Packet, signing::KeyStore, util::debounce};

mod combinators;
mod network;
pub mod proto;

// TODO: clean this up

pub struct Discover {
    msg_tx: mpsc::Sender<Packet>,
}

struct DiscoverCtx {
    config: Config,
    keystore: KeyStore,
    event_tx: mpsc::Sender<Event>,
}

pub enum Event {
    Goodbye {
        source_ip: IpAddr,
    },
    Adv {
        source_ip: IpAddr,
        binary_cache_url: Url,
    },
}

impl Discover {
    pub async fn run(
        set: &mut JoinSet<Result<(), eyre::Error>>,
        config: Config,
        keystore: KeyStore,
        token: CancellationToken,
    ) -> Result<(Self, mpsc::Receiver<Event>), eyre::Error> {
        let (event_tx, event_rx) = mpsc::channel(1);
        let (msg_tx, mut msg_rx) = mpsc::channel::<Packet>(1);

        let (network, addrs_changed) = network::Network::new().await?;

        let mut addrs_changed = debounce(addrs_changed, Duration::from_millis(500));

        let ctx = Arc::new(DiscoverCtx {
            config,
            keystore,
            event_tx: event_tx.clone(),
        });

        let sock = UdpSocket::bind(("0.0.0.0", proto::PORT)).await?;
        sock.set_broadcast(true)?;

        set.spawn({
            // TODO: reformat this/subdivide into functions this is horrible
            async move {
                let mut buf = vec![0; proto::PACKET_MAXIMUM_REASONABLE_SIZE];
                loop {
                    let mut managed = HashMap::default();

                    for (ip, broadcast) in network.find_eligible_ips().await.expect("Can't discover IPs") {
                        tracing::info!(%ip, broadcast_addr = %broadcast, "Found interface");
                        managed.insert(ip, broadcast);
                    }

                    loop {
                        tokio::select! {
                            _ = token.cancelled() => {
                                if ctx.keystore.has_private_key() {
                                    for broadcast in managed.values() {
                                        tracing::info!(addr = %broadcast, "Sending goodbye");
                                        sock.send_to(&Packet::Goodbye.into_bytes(&ctx.keystore)?, (IpAddr::V4(*broadcast), proto::PORT)).await?;
                                    }
                                }
                                return Ok(());
                            }
                            _ = addrs_changed.recv() => {
                                break;
                            }
                            Some(packet) = msg_rx.recv() => {
                                for broadcast in managed.values() {
                                    tracing::debug!(?packet, "Sending");
                                    // TODO: avoid clone
                                    sock.send_to(&packet.clone().into_bytes(&ctx.keystore)?, (IpAddr::V4(*broadcast), proto::PORT)).await?;
                                }
                            }
                            res = sock.recv_from(&mut buf[..]) => {
                                match res {
                                    Ok((len, sock_addr)) => {
                                        if managed.contains_key(&sock_addr.ip()) {
                                            continue;
                                        }
                                        if len > buf.len() {
                                            tracing::debug!(from = %sock_addr, %len, "Dropping oversized packet");
                                            continue;
                                        }
                                        let packet = &buf[..len];
                                        match Packet::parse(packet, &ctx.keystore) {
                                            Ok(Some(packet)) => {
                                                tracing::debug!(packet = ?packet, from = %sock_addr, "Received packet");
                                                match packet {
                                                    Packet::Req => {
                                                        if let Some(cache) = ctx.config.local_binary_caches.as_ref().and_then(|entry| entry.local_cache.as_ref()) {
                                                            for broadcast in managed.values() {
                                                                sock.send_to(
                                                                    &Packet::Adv {
                                                                        binary_cache: cache.clone(),
                                                                    }
                                                                    .into_bytes(&ctx.keystore)?,
                                                                    (IpAddr::V4(*broadcast), proto::PORT),
                                                                )
                                                                .await?;
                                                            }
                                                        }
                                                    }
                                                    Packet::Adv {
                                                        binary_cache,
                                                    } => {
                                                        let binary_cache_url = match binary_cache {
                                                            LocalCache::Url { url } => url,
                                                            LocalCache::Ip { port } => Url::parse(&format!("http://{}:{}", sock_addr.ip(), port)).unwrap(),
                                                        };
                                                        if ctx.event_tx.send(Event::Adv { source_ip: sock_addr.ip(), binary_cache_url }).await.is_err() {
                                                            break;
                                                        }
                                                    }
                                                    Packet::Goodbye => {
                                                        if ctx.event_tx.send(Event::Goodbye { source_ip: sock_addr.ip()}).await.is_err() {
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                            Ok(None) => {}
                                            Err(e) => {
                                                tracing::warn!(from = %sock_addr, error = %e, "Received invalid packet");
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        // FIXME:
                                        panic!("wut do? {}", e);
                                    }
                                }
                            }
                            else => return Ok(()),
                        }
                    }
                }
        }});

        Ok((Self { msg_tx }, event_rx))
    }

    pub async fn broadcast_req(&self) {
        self.msg_tx.send(Packet::Req).await.unwrap();
    }
}
