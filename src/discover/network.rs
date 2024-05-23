use std::net::{IpAddr, Ipv4Addr};

use eyre::Context;
use futures::{StreamExt, TryStreamExt};
use netlink_packet_route::{
    address::{AddressAttribute, AddressMessage},
    link::{LinkExtentMask, LinkFlag, LinkLayerType},
};
use netlink_sys::AsyncSocket;
use rtnetlink::constants::{RTMGRP_IPV4_IFADDR, RTMGRP_IPV6_IFADDR};
use tokio::sync::mpsc;

pub struct Network {
    handle: rtnetlink::Handle,
}

// TODO: abstract this if this should ever work on other Unixen
impl Network {
    pub async fn new() -> Result<(Self, mpsc::Receiver<()>), eyre::Error> {
        let (mut conn, handle, mut messages) =
            rtnetlink::new_connection().context("Failed creating netlink socket")?;
        conn.socket_mut()
            .socket_mut()
            .bind(&netlink_sys::SocketAddr::new(
                0,
                RTMGRP_IPV4_IFADDR | RTMGRP_IPV6_IFADDR,
            ))
            .context("Failed binding socket")?;
        tokio::spawn(conn);

        let (tx, rx) = mpsc::channel(1);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(msg) = messages.next() => {
                        if tx.send(()).await.is_err() {
                            return;
                        }
                        tracing::debug!(change = ?msg, "Received interface change");
                    }
                    else => return,
                }
            }
        });

        Ok((Network { handle }, rx))
    }

    pub async fn find_eligible_ips(&self) -> Result<Vec<(IpAddr, Ipv4Addr)>, eyre::Error> {
        let mut ret = Vec::new();
        // FIXME: too much cloning going on for no reason
        let addresses = self.handle.clone().address();

        // no idea how ipv6 works so just ignore it
        let mut interfaces = self
            .handle
            .clone()
            .link()
            .get()
            .set_filter_mask(
                netlink_packet_route::AddressFamily::Inet,
                vec![
                    // skip stats that are never used for shorter reply
                    LinkExtentMask::SkipStats,
                ],
            )
            .execute();
        while let Some(resp) = interfaces.try_next().await? {
            let header = &resp.header;
            if header.link_layer_type == LinkLayerType::Ether
            // why isn't this a bitfield?
            && header.flags.contains(&LinkFlag::Running)
            && header.flags.contains(&LinkFlag::Broadcast)
            {
                let mut addrs = addresses
                    .get()
                    .set_link_index_filter(header.index)
                    .execute();
                while let Some(resp) = addrs.try_next().await? {
                    if let Some(ips) = extract_ip(&resp) {
                        ret.push(ips);
                    }
                }
            }
        }
        Ok(ret)
    }
}

fn extract_ip(msg: &AddressMessage) -> Option<(IpAddr, Ipv4Addr)> {
    let mut ip = None;
    let mut broadcast = None;
    for attr in &msg.attributes {
        match attr {
            AddressAttribute::Address(addr) => ip = Some(*addr),
            AddressAttribute::Broadcast(addr) => broadcast = Some(*addr),
            _ => (),
        }
    }
    match (ip, broadcast) {
        (Some(ip), Some(broadcast)) => Some((ip, broadcast)),
        _ => None,
    }
}
