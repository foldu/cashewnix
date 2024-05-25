use std::num::NonZeroU16;

use super::combinators::{tag, take, u64_le, u8};
use ring_compat::signature::ed25519::Signature;
use url::Url;

use crate::signing::{KeyStore, VerificationError};

#[derive(serde::Deserialize, serde::Serialize, Debug)]
enum SignedPacketContent {
    Adv { binary_cache: LocalCache },
    Goodbye,
}

#[derive(serde::Deserialize, serde::Serialize, Debug)]
enum UntrustedPacketContent {
    Req,
}

#[derive(serde::Serialize, Eq, PartialEq, serde::Deserialize, Debug, Clone)]
#[serde(tag = "advertise", rename_all = "snake_case")]
pub enum LocalCache {
    Url { url: Url },
    Ip { port: NonZeroU16 },
}

// TODO: maybe use protobuf instead, it'd be kind of annoying if all local caches stopped working
// after an update because I haven't thought about backwards compatibility

const MAGIC: &[u8] = b"cashewnix";
const UDP_MAX_DATAGRAM_SIZE: usize = 65507;
pub const PORT: u16 = 49745;
// already pretty big
pub const PACKET_MAXIMUM_REASONABLE_SIZE: usize = 1024;

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum Packet {
    Adv { binary_cache: LocalCache },
    Goodbye,
    Req,
}

struct RawPacket<'a> {
    signature: Option<Signature>,
    content: &'a [u8],
}

impl<'a> RawPacket<'a> {
    fn parse(buf: &'a [u8]) -> Option<Self> {
        let (buf, _) = tag(buf, MAGIC)?;

        let (buf, has_signature) = u8(buf)?;

        let (buf, signature) = if has_signature == 0xff {
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

#[derive(thiserror::Error, Debug)]
pub enum ParseError {
    #[error("Can't parse package header")]
    InvalidHeader,
    #[error("Failed verifying package body")]
    Verify {
        #[from]
        source: VerificationError,
    },
    #[error("Timestamp of package not current enough")]
    TimestampDiff,

    #[error("Can't parse package body")]
    ParseBody {
        #[from]
        source: rmp_serde::decode::Error,
    },
}

#[derive(thiserror::Error, Debug)]
pub enum SerializeError {
    #[error("Package too big")]
    PackageTooBig { size: usize },

    #[error("Missing private key to sign message")]
    MissingKey,
}

const MAX_PACKAGE_TIME_DIFF: u64 = 5;
const REALISTIC_MESSAGE_SIZE: usize = 128;
const TIMESTAMP_SIZE: usize = std::mem::size_of::<u64>();

impl Packet {
    pub fn parse(buf: &[u8], keystore: &KeyStore) -> Result<Packet, ParseError> {
        let Some(raw) = RawPacket::parse(buf) else {
            return Err(ParseError::InvalidHeader);
        };

        match raw.signature {
            Some(sig) => {
                keystore.verify(&sig, raw.content)?;
                let Some((msg, timestamp)) = u64_le(raw.content) else {
                    return Err(ParseError::InvalidHeader);
                };

                let now = current_timestamp();
                let diff = if now < timestamp {
                    timestamp - now
                } else {
                    now - timestamp
                };

                if diff > MAX_PACKAGE_TIME_DIFF {
                    return Err(ParseError::TimestampDiff);
                }

                let content: SignedPacketContent = rmp_serde::from_slice(msg)?;
                Ok(match content {
                    SignedPacketContent::Adv { binary_cache } => Packet::Adv { binary_cache },
                    SignedPacketContent::Goodbye => Packet::Goodbye,
                })
            }
            None => {
                let content: UntrustedPacketContent = rmp_serde::from_slice(raw.content)?;
                Ok(match content {
                    UntrustedPacketContent::Req => Packet::Req,
                })
            }
        }
    }

    pub fn to_bytes(&self, keystore: &KeyStore) -> Result<Vec<u8>, SerializeError> {
        // NOTE: always needs to clone because SignedPacketContent etc.
        // would need to be repeated for reference types
        let kind = PacketKind::from(self.clone());
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
                let signature = keystore.sign(&body).ok_or(SerializeError::MissingKey)?;
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
            return Err(SerializeError::PackageTooBig { size });
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
            let bytes = packet.to_bytes(&keystore).unwrap();
            assert!(bytes.len() < PACKET_MAXIMUM_REASONABLE_SIZE);
            assert_eq!(Packet::parse(&bytes, &keystore).unwrap(), original);
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
