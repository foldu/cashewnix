use base64::Engine;
use eyre::{Context, ContextCompat};
use ring_compat::signature::{
    ed25519::{Signature, SigningKey, VerifyingKey},
    Signer,
    Verifier,
};

#[derive(Default)]
pub struct KeyStore {
    public: Vec<VerifyingKey>,
    private: Option<SigningKey>,
}

#[derive(thiserror::Error, Debug)]
#[error("Failed verifying signature with any public key")]
pub struct VerificationError;

impl KeyStore {
    pub fn add_public_key(&mut self, public_key: VerifyingKey) {
        // NOTE: VerifyingKey doesn't implement Hash or Ord so need to do it like this
        // probably no noticeable performance impact, this thing isn't called a thousand times
        if !self.public.contains(&public_key) {
            self.public.push(public_key);
        }
    }

    pub fn init_private_key(&mut self, key: SigningKey) {
        self.private = Some(key);
    }

    pub fn sign(&self, msg: &[u8]) -> Option<Signature> {
        self.private.as_ref().map(|pair| pair.sign(msg))
    }

    pub fn verify(&self, signature: &Signature, msg: &[u8]) -> Result<(), VerificationError> {
        for key in &self.public {
            if key.verify(msg, signature).is_ok() {
                return Ok(());
            }
        }

        Err(VerificationError)
    }

    pub fn has_private_key(&self) -> bool {
        self.private.is_some()
    }
}

fn parse_ssh_key(key: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let trimmed = match key.split_once(':') {
        Some((_, key)) => key,
        None => key,
    };
    base64::engine::general_purpose::STANDARD.decode(trimmed)
}

pub fn parse_ssh_private_key(key: &str) -> Result<SigningKey, eyre::Error> {
    let err = || eyre::format_err!("Failed decoding key {}", key);
    let key = parse_ssh_key(key).with_context(err)?;

    let key = key.get(0..32).with_context(err)?;
    SigningKey::from_slice(key).map_err(|_| err())
}

pub fn parse_ssh_public_key(public_key: &str) -> Result<VerifyingKey, eyre::Error> {
    let err = || eyre::format_err!("Failed decoding key {}", public_key);
    let key = parse_ssh_key(public_key).with_context(err)?;
    VerifyingKey::from_slice(&key).map_err(|_| err())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystore_works() {
        let private = SigningKey::generate(&mut rand_core::OsRng);
        let public = private.verifying_key();

        let mut keystore = KeyStore::default();

        keystore.add_public_key(public);
        keystore.init_private_key(private);

        keystore_test(&keystore);
    }

    fn keystore_test(keystore: &KeyStore) {
        let msg = b"test";

        let sig = keystore.sign(msg).unwrap();
        assert!(keystore.verify(&sig, msg).is_ok());
    }

    #[test]
    fn parse_ssh_key_works() {
        let mut keystore = KeyStore::default();
        let public = std::fs::read_to_string("data/public").unwrap();
        let private = std::fs::read_to_string("data/private").unwrap();
        let public = parse_ssh_public_key(&public).unwrap();
        let private = parse_ssh_private_key(&private).unwrap();

        keystore.add_public_key(public);
        keystore.init_private_key(private);
        keystore_test(&keystore);
    }
}
