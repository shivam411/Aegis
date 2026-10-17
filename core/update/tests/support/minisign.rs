//! A throwaway minisign key pair for tests, generated at test time (never
//! committed). Produces the same files as `minisign -G` / `minisign -S`:
//! Ed25519 over the BLAKE2b-512 hash of the file ("ED", pre-hashed), plus
//! the global signature over the signature and the trusted comment.
#![allow(dead_code)]

use base64::Engine;
use blake2::{Blake2b512, Digest};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};

pub struct TestKey {
    pair: Ed25519KeyPair,
    key_id: [u8; 8],
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

impl TestKey {
    pub fn generate() -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut key_id = [0u8; 8];
        rng.fill(&mut key_id).unwrap();
        Self { pair, key_id }
    }

    pub fn raw_id(&self) -> [u8; 8] {
        self.key_id
    }

    /// The key ID as minisign prints it.
    pub fn id(&self) -> String {
        format!("{:016X}", u64::from_le_bytes(self.key_id))
    }

    /// The second line of the `.pub` file.
    pub fn public_base64(&self) -> String {
        let mut raw = b"Ed".to_vec();
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(self.pair.public_key().as_ref());
        b64(&raw)
    }

    fn signature_file(&self, algorithm: &[u8; 2], key_id: [u8; 8], message: &[u8], comment: &str) -> String {
        let signature = self.pair.sign(message);
        let mut line = algorithm.to_vec();
        line.extend_from_slice(&key_id);
        line.extend_from_slice(signature.as_ref());
        let mut global = signature.as_ref().to_vec();
        global.extend_from_slice(comment.as_bytes());
        let global = self.pair.sign(&global);
        format!(
            "untrusted comment: signature from test key\n{}\ntrusted comment: {}\n{}\n",
            b64(&line),
            comment,
            b64(global.as_ref())
        )
    }

    /// `minisign -S -t <comment>`: a `.minisig` file for `data`.
    pub fn sign(&self, data: &[u8], comment: &str) -> String {
        self.sign_as(data, comment, self.key_id)
    }

    /// A signature by this key that claims to be from `key_id`.
    pub fn sign_as(&self, data: &[u8], comment: &str, key_id: [u8; 8]) -> String {
        let hash = Blake2b512::digest(data);
        self.signature_file(b"ED", key_id, &hash, comment)
    }

    /// `minisign -S -l`: a legacy (not pre-hashed) signature.
    pub fn sign_legacy(&self, data: &[u8], comment: &str) -> String {
        self.signature_file(b"Ed", self.key_id, data, comment)
    }
}
