use crate::discover::proto::LocalCache;
use ahash::HashMap;
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    time::Duration,
};
use url::Url;

use tokio::{net::UdpSocket, sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;

use crate::{config::Config, discover::proto::Packet, signing::KeyStore, util::debounce};

mod combinators;
mod network;
pub mod proto;

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

        set.spawn({
            let msg_tx = msg_tx.clone();
            async move {
            let mut join_set = JoinSet::new();
            let child_token = token.child_token();

            loop {
                let mut managed = HashMap::default();

                for (ip, broadcast) in network.find_eligible_ips().await? {
                    let tx = listener_task(
                        &mut join_set,
                        ip,
                        broadcast,
                        ctx.clone(),
                        child_token.clone(),
                    )
                    .await?;
                    managed.insert(ip, (broadcast, tx));
                }

                msg_tx.send(Packet::Req).await.unwrap();

                loop {
                    tokio::select! {
                        _ = token.cancelled() => {
                            while join_set.join_next().await.is_some() {}
                            return Ok(());
                        }
                        _ = addrs_changed.recv() => {
                            join_set.shutdown().await;
                            break;
                        }
                        Some(msg) = msg_rx.recv() => {
                            for (broadcast, tx) in managed.values() {
                                if tx.send((msg.clone(), IpAddr::V4(*broadcast))).await.is_err() {
                                    tracing::error!(?broadcast, "Failed sending to network task, probably crashed");
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

async fn listener_task(
    join_set: &mut JoinSet<Result<(), eyre::Error>>,
    ip: IpAddr,
    broadcast: Ipv4Addr,
    ctx: Arc<DiscoverCtx>,
    token: CancellationToken,
) -> Result<mpsc::Sender<(proto::Packet, IpAddr)>, eyre::Error> {
    let sock = UdpSocket::bind(("0.0.0.0", proto::PORT)).await?;
    sock.set_broadcast(true)?;

    let (tx, mut rx) = mpsc::channel::<(proto::Packet, IpAddr)>(1);
    join_set.spawn(async move {
        let mut buf = vec![0; proto::PACKET_MAXIMUM_REASONABLE_SIZE];
        loop {
            // FIXME: this needs proper formatting
            tokio::select! {
                _ = token.cancelled() => {
                    tracing::info!(addr = %broadcast, "Sending goodbye");
                    sock.send_to(&Packet::Goodbye.into_bytes(&ctx.keystore)?, (broadcast, proto::PORT)).await?;
                    break;
                }
                Some((packet, addr)) = rx.recv() => {
                    tracing::debug!(?packet, "Sending");
                    sock.send_to(&packet.into_bytes(&ctx.keystore)?, (addr, proto::PORT)).await?;
                }
                res = sock.recv_from(&mut buf[..]) => {
                    match res {
                        Ok((len, sock_addr)) => {
                            if sock_addr.ip() == ip {
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
                                                sock.send_to(
                                                    &Packet::Adv {
                                                        binary_cache: cache.clone(),
                                                    }
                                                    .into_bytes(&ctx.keystore)?,
                                                    (broadcast, proto::PORT),
                                                )
                                                .await?;
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
            }
        }

        Ok(())
    });

    Ok(tx)
}
