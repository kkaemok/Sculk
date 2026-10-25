//! Documented LOCO type-16/type-3 exchange. Public-key encryption only.
//! The caller must authenticate the provenance of the server public key. Sending
//! this handshake is not server confirmation; a valid GCM response is required.
use crate::{secure::GcmRecords, Error};
use rand::{rngs::OsRng, RngCore};
use rsa::{pkcs1::DecodeRsaPublicKey, traits::PublicKeyParts, Oaep, RsaPublicKey};
use sha1::Sha1;
use zeroize::Zeroizing;

/// Caller-trusted RSA-2048 public key, never discovered from an untrusted peer.
#[derive(Clone)]
pub struct ServerKey(RsaPublicKey);
impl ServerKey {
    /// Import bounded PKCS#1 DER. The documented exchange requires 2048 bits.
    /// This validates structure/size, not the key's identity or provenance.
    pub fn from_pkcs1_der(der: &[u8]) -> Result<Self, Error> {
        if der.len() > 4096 {
            return Err(Error::Crypto);
        }
        let key = RsaPublicKey::from_pkcs1_der(der).map_err(|_| Error::Crypto)?;
        if key.n().bits() != 2048 {
            return Err(Error::Crypto);
        }
        Ok(Self(key))
    }
    /// Generate a fresh AES-128 key and OAEP ciphertext with operating-system
    /// randomness. Wire: LE u32 256, 16, 3, then 256 encrypted key bytes.
    /// SHA-1 is protocol mandated here, not a general cryptographic recommendation.
    pub fn initiate(&self) -> Result<([u8; 268], GcmRecords), Error> {
        let mut key = Zeroizing::new([0u8; 16]);
        OsRng
            .try_fill_bytes(key.as_mut())
            .map_err(|_| Error::Crypto)?;
        let encrypted = self
            .0
            .encrypt(&mut OsRng, Oaep::new::<Sha1>(), key.as_ref())
            .map_err(|_| Error::Crypto)?;
        let mut wire = [0u8; 268];
        wire[..4].copy_from_slice(&256u32.to_le_bytes());
        wire[4..8].copy_from_slice(&16u32.to_le_bytes());
        wire[8..12].copy_from_slice(&3u32.to_le_bytes());
        wire[12..].copy_from_slice(&encrypted);
        Ok((wire, GcmRecords::new(&key)))
    }
}
