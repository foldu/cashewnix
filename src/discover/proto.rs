use std::num::NonZeroU16;

use super::combinators::{tag, take, u64_le, u8};
use eyre::ContextCompat;
use ring_compat::signature::ed25519::Signature;
use url::Url;

use crate::signing::KeyStore;

// TODO: maybe use protobuf instead, it'd be kind of annoying if all local caches stopped working
// after an update because I haven't thought about backwards compatibility

const MAGIC: &[u8] = b"cashewnix";
const UDP_MAX_DATAGRAM_SIZE: usize = 65507;
pub const PORT: u16 = 49745;
// already pretty big
pub const PACKET_MAXIMUM_REASONABLE_SIZE: usize = 1024;

#[derive(serde::Serialize, Eq, PartialEq, serde::Deserialize, Debug, Clone)]
#[serde(tag = "advertise", rename_all = "snake_case")]
pub enum LocalCache {
    Url { url: Url },
    Ip { port: NonZeroU16 },
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum Packet {
    Adv { binary_cache: LocalCache },
    Goodbye,
    Req,
}

#[derive(serde::Deserialize, serde::Serialize, Debug)]
pub enum SignedPacketContent {
    Adv { binary_cache: LocalCache },
    Goodbye,
}

#[derive(serde::Deserialize, serde::Serialize, Debug)]
pub enum UntrustedPacketContent {
    Req,
}

struct RawPacket<'a> {
    signature: Option<Signature>,
    content: &'a [u8],
}

impl<'a> RawPacket<'a> {
    fn parse(buf: &'a [u8]) -> Option<Self> {
        let (buf, _) = tag(buf, MAGIC)?;

        let (buf, has_key) = u8(buf)?;

        let (buf, signature) = if has_key == 0xff {
            let (buf, signature) = take(buf, Signature::BYTE_SIZE)?;
            let signature = Signature::from_slice(signature).ok()?;
            (buf, Some(signature))
        } else {
            (buf, None)
        };

        Some(RawPacket {
            signature,
            content: buf,
        })
    }
}

enum PacketKind {
    Signed(SignedPacketContent),
    Untrusted(UntrustedPacketContent),
}

impl From<Packet> for PacketKind {
    fn from(value: Packet) -> Self {
        match value {
            Packet::Adv { binary_cache } => {
                PacketKind::Signed(SignedPacketContent::Adv { binary_cache })
            }
            Packet::Goodbye => PacketKind::Signed(SignedPacketContent::Goodbye),
            Packet::Req => PacketKind::Untrusted(UntrustedPacketContent::Req),
        }
    }
}

const MAX_PACKAGE_AGE: u64 = 5;
const REALISTIC_MESSAGE_SIZE: usize = 128;
const TIMESTAMP_SIZE: usize = std::mem::size_of::<u64>();

impl Packet {
    pub fn parse(buf: &[u8], keystore: &KeyStore) -> Result<Option<Packet>, eyre::Error> {
        let Some(raw) = RawPacket::parse(buf) else {
            return Ok(None);
        };

        match raw.signature {
            Some(sig) => {
                keystore.verify(&sig, raw.content)?;
                let Some((msg, timestamp)) = u64_le(raw.content) else {
                    eyre::bail!("FIXME");
                };

                match current_timestamp().checked_sub(timestamp) {
                    Some(n) if n > MAX_PACKAGE_AGE => eyre::bail!("Package too old"),
                    None => eyre::bail!("Package from the future"),
                    _ => (),
                }

                let content: SignedPacketContent = rmp_serde::from_slice(msg)?;
                Ok(Some(match content {
                    SignedPacketContent::Adv { binary_cache } => Packet::Adv { binary_cache },
                    SignedPacketContent::Goodbye => Packet::Goodbye,
                }))
            }
            None => {
                let content: UntrustedPacketContent = rmp_serde::from_slice(raw.content)?;
                Ok(Some(match content {
                    UntrustedPacketContent::Req => Packet::Req,
                }))
            }
        }
    }

    pub fn into_bytes(self, keystore: &KeyStore) -> Result<Vec<u8>, eyre::Error> {
        let kind = PacketKind::from(self);
        let mut ret: Vec<u8> = Vec::with_capacity(
            MAGIC.len()
                // signature indicator
                + 1
                + match kind {
                    PacketKind::Signed(_) => Signature::BYTE_SIZE + TIMESTAMP_SIZE + REALISTIC_MESSAGE_SIZE,
                    PacketKind::Untrusted(_) => REALISTIC_MESSAGE_SIZE,
                },
        );
        ret.extend(MAGIC);
        let buf = match kind {
            PacketKind::Signed(content) => {
                ret.push(0xff);
                let mut body = Vec::with_capacity(TIMESTAMP_SIZE + REALISTIC_MESSAGE_SIZE);
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    .to_le_bytes();
                body.extend(timestamp);

                let buf = rmp_serde::to_vec(&content).unwrap();
                body.extend(buf);
                let signature = keystore
                    .sign(&body)
                    .context("Missing private key for this package kind")?;
                ret.extend(signature.to_bytes());
                body
            }
            PacketKind::Untrusted(content) => {
                ret.push(0x00);
                rmp_serde::to_vec(&content).unwrap()
            }
        };
        ret.extend(&buf);

        let size = ret.len();
        if size > UDP_MAX_DATAGRAM_SIZE {
            eyre::bail!("Packet bigger than maximum UDP datagram");
        }
        if size > PACKET_MAXIMUM_REASONABLE_SIZE {
            tracing::warn!(size, "Created packet above reasonable size");
        }
        Ok(ret)
    }
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use crate::signing::parse_ssh_private_key;

    use super::*;
    #[test]
    fn packet_roundtrip() {
        let mut keystore = KeyStore::default();
        let key = parse_ssh_private_key(include_str!("../../data/private")).unwrap();
        keystore.add_public_key(key.verifying_key());
        keystore.init_private_key(key);

        let roundtrip = |packet: Packet| {
            let original = packet.clone();
            let bytes = packet.into_bytes(&keystore).unwrap();
            assert!(bytes.len() < PACKET_MAXIMUM_REASONABLE_SIZE);
            assert_eq!(Packet::parse(&bytes, &keystore).unwrap(), Some(original));
        };
        roundtrip(Packet::Req);
        roundtrip(Packet::Goodbye);
        roundtrip(Packet::Adv {
            binary_cache: LocalCache::Ip {
                port: NonZeroU16::new(5).unwrap(),
            },
        });
    }
}
