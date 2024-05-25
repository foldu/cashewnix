use crate::discover::proto::LocalCache;
use ahash::HashMap;
use eyre::Context;
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use url::Url;

use tokio::{net::UdpSocket, sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;

use crate::{config::Config, discover::proto::Packet, signing::KeyStore, util::debounce};

use self::proto::ParseError;

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
    managed: HashMap<IpAddr, NetData>,
}

struct NetData {
    broadcast_addr: IpAddr,
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("event_tx hung up")]
    Hungup,
    #[error("Missing private key")]
    MissingKey,
    #[error("Failed sending")]
    SocketSend {
        #[source]
        source: std::io::Error,
    },
}

impl DiscoverCtx {
    async fn handle_datagram(
        &self,
        buf: &[u8],
        sock_addr: SocketAddr,
        sock: &UdpSocket,
    ) -> Result<(), Error> {
        match Packet::parse(buf, &self.keystore) {
            Ok(packet) => {
                tracing::debug!(packet = ?packet, from = %sock_addr, "Received packet");
                match packet {
                    Packet::Req => {
                        if let Some(cache) = self
                            .config
                            .local_binary_caches
                            .as_ref()
                            .and_then(|entry| entry.local_cache.as_ref())
                        {
                            for net in self.managed.values() {
                                sock.send_to(
                                    &Packet::Adv {
                                        binary_cache: cache.clone(),
                                    }
                                    .to_bytes(&self.keystore)
                                    .map_err(|_| Error::MissingKey)?,
                                    (net.broadcast_addr, proto::PORT),
                                )
                                .await
                                .map_err(|e| Error::SocketSend { source: e })?;
                            }
                        }
                    }
                    Packet::Adv { binary_cache } => {
                        let binary_cache_url = match binary_cache {
                            LocalCache::Url { url } => url,
                            LocalCache::Ip { port } => {
                                Url::parse(&format!("http://{}:{}", sock_addr.ip(), port)).unwrap()
                            }
                        };
                        if self
                            .event_tx
                            .send(Event::Adv {
                                source_ip: sock_addr.ip(),
                                binary_cache_url,
                            })
                            .await
                            .is_err()
                        {
                            return Err(Error::Hungup);
                        }
                    }
                    Packet::Goodbye => {
                        if self
                            .event_tx
                            .send(Event::Goodbye {
                                source_ip: sock_addr.ip(),
                            })
                            .await
                            .is_err()
                        {
                            return Err(Error::Hungup);
                        }
                    }
                }
            }
            // ignore all packets that don't have a proper header
            Err(ParseError::InvalidHeader) => {}
            Err(e) => {
                tracing::warn!(from = %sock_addr, error = %e, "Received invalid packet");
            }
        }
        Ok(())
    }

    async fn send_goodbye(&self, sock: &UdpSocket) -> Result<(), eyre::Error> {
        if self.keystore.has_private_key() {
            for net in self.managed.values() {
                tracing::info!(addr = %net.broadcast_addr, "Sending goodbye");
                sock.send_to(
                    &Packet::Goodbye.to_bytes(&self.keystore)?,
                    (net.broadcast_addr, proto::PORT),
                )
                .await?;
            }
        }

        Ok(())
    }
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

        let mut ctx = DiscoverCtx {
            config,
            keystore,
            event_tx: event_tx.clone(),
            managed: Default::default(),
        };

        let sock = UdpSocket::bind(("0.0.0.0", proto::PORT))
            .await
            .context("Failed binding discover socket")?;
        sock.set_broadcast(true)?;

        set.spawn(async move {
                let mut buf = vec![0; proto::PACKET_MAXIMUM_REASONABLE_SIZE];
                loop {
                    ctx.managed.clear();

                    // TODO: maybe retry again later instead of panicking
                    for (ip, broadcast) in network.find_eligible_ips().await.expect("Can't discover IPs") {
                        tracing::info!(%ip, broadcast_addr = %broadcast, "Found interface");
                        ctx.managed.insert(
                            ip,
                            NetData {
                                broadcast_addr: IpAddr::V4(broadcast),
                            },
                        );
                    }

                    loop {
                        tokio::select! {
                            _ = token.cancelled() => {
                                ctx.send_goodbye(&sock).await?;
                                return Ok(());
                            }
                            _ = addrs_changed.recv() => {
                                break;
                            }
                            Some(packet) = msg_rx.recv() => {
                                for net in ctx.managed.values() {
                                    tracing::debug!(?packet, "Sending");
                                    sock.send_to(&packet.to_bytes(&ctx.keystore)?, (net.broadcast_addr, proto::PORT)).await?;
                                }
                            }
                            res = sock.recv_from(&mut buf[..]) => {
                                match res {
                                    Ok((len, sock_addr)) => {
                                        if ctx.managed.contains_key(&sock_addr.ip()) {
                                            continue;
                                        }
                                        if len > buf.len() {
                                            tracing::debug!(from = %sock_addr, %len, "Dropping oversized packet");
                                            continue;
                                        }
                                        if let Err(e) = ctx.handle_datagram(&buf[..len], sock_addr, &sock).await {
                                            match e {
                                                Error::Hungup => {
                                                    return Ok(());
                                                }
                                                Error::SocketSend { source } => {
                                                    tracing::error!(error = %source, "Failed sending");
                                                }
                                                Error::MissingKey => {
                                                    tracing::error!("Tried to sign a packet without a private key");
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        // when can this even happen?
                                        tracing::error!(error = %e, "Error receiving from socket");
                                    }

                                }
                            }
                            else => return Ok(()),
                        }
                    }
                }
        });

        Ok((Self { msg_tx }, event_rx))
    }

    pub async fn broadcast_req(&self) {
        self.msg_tx.send(Packet::Req).await.unwrap();
    }
}
