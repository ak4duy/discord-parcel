use anyhow::{Result, anyhow, ensure};
use ring::{
    aead, pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

pub const OVERHEAD: u64 = 28;
const SCHEME: &str = "aes-256-gcm-pbkdf2-sha256-600000";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Encryption {
    pub scheme: String,
    pub salt: [u8; 16],
    pub check: Vec<u8>,
}

impl Encryption {
    pub fn new(password: &str) -> Result<Self> {
        ensure!(!password.is_empty(), "Enter an encryption passphrase.");
        let mut salt = [0; 16];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| anyhow!("Could not generate encryption salt."))?;
        let mut metadata = Self {
            scheme: SCHEME.into(),
            salt,
            check: Vec::new(),
        };
        metadata.check = metadata
            .derive(password)?
            .seal(b"discord-parcel-key-check", b"key-check")?;
        Ok(metadata)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.scheme == SCHEME && self.check.len() == 52,
            "Unsupported encryption metadata."
        );
        Ok(())
    }

    fn derive(&self, password: &str) -> Result<Key> {
        ensure!(
            !password.is_empty(),
            "This parcel is encrypted. Enter its passphrase."
        );
        let mut bytes = [0; 32];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(600_000).unwrap(),
            &self.salt,
            password.as_bytes(),
            &mut bytes,
        );
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, &bytes)
            .map_err(|_| anyhow!("Could not create encryption key."))?;
        bytes.fill(0);
        Ok(Key(aead::LessSafeKey::new(key)))
    }

    pub fn unlock(&self, password: &str) -> Result<Key> {
        self.validate()?;
        let key = self.derive(password)?;
        ensure!(
            key.open(&self.check, b"key-check")? == b"discord-parcel-key-check",
            "Incorrect passphrase."
        );
        Ok(key)
    }
}

pub struct Key(aead::LessSafeKey);

impl Key {
    pub fn seal(&self, bytes: &[u8], context: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| anyhow!("Could not generate encryption nonce."))?;
        let mut ciphertext = bytes.to_vec();
        self.0
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(context),
                &mut ciphertext,
            )
            .map_err(|_| anyhow!("Could not encrypt part."))?;
        let mut output = nonce.to_vec();
        output.extend(ciphertext);
        Ok(output)
    }

    pub fn open(&self, bytes: &[u8], context: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            bytes.len() >= OVERHEAD as usize,
            "Encrypted part is truncated."
        );
        let nonce: [u8; 12] = bytes[..12].try_into()?;
        let mut plaintext = bytes[12..].to_vec();
        let opened = self
            .0
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(context),
                &mut plaintext,
            )
            .map_err(|_| anyhow!("Incorrect passphrase or damaged encrypted parcel."))?;
        Ok(opened.to_vec())
    }
}

pub fn context(manifest: &crate::parcel::Manifest, index: usize) -> Vec<u8> {
    format!(
        "{}:{}:{}:{}:{}:{}",
        manifest.id, manifest.filename, manifest.size, manifest.chunk_size, manifest.sha256, index
    )
    .into_bytes()
}
