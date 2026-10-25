//! Experimental type-3 record layout observed in openkakao-cli, not live verified.
//! Key establishment and peer identity verification are deliberately out of scope.
//! Never treat symmetric roundtrip tests as proof of server compatibility.
use crate::{Error, DEFAULT_MAX_BODY, HEADER_LEN};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes128Gcm, Nonce,
};
use bytes::{BufMut, Bytes, BytesMut};
use rand::RngCore;
use tokio_util::codec::{Decoder, Encoder};

/// AES-128-GCM records: LE u32 length, 12-byte nonce, ciphertext, 16-byte tag.
/// No key accessors or Debug output. Per-instance record count bounds random
/// nonce use. Create a new negotiated key before reaching the limit.
pub struct GcmRecords {
    cipher: Aes128Gcm,
    sent: u64,
}
impl GcmRecords {
    /// Use a caller-established key. Does not negotiate or authenticate a peer.
    pub fn new(key: &[u8; 16]) -> Self {
        Self {
            cipher: Aes128Gcm::new(key.into()),
            sent: 0,
        }
    }
}
impl Encoder<Bytes> for GcmRecords {
    type Error = Error;
    fn encode(&mut self, plaintext: Bytes, dst: &mut BytesMut) -> Result<(), Error> {
        if plaintext.len() > DEFAULT_MAX_BODY + HEADER_LEN || self.sent >= (1 << 24) {
            return Err(Error::Length(plaintext.len()));
        }
        let mut nonce = [0; 12];
        rand::rngs::OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| Error::Crypto)?;
        let encrypted = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
            .map_err(|_| Error::Crypto)?;
        self.sent += 1;
        dst.put_u32_le((12 + encrypted.len()) as u32);
        dst.extend_from_slice(&nonce);
        dst.extend_from_slice(&encrypted);
        Ok(())
    }
}
impl Decoder for GcmRecords {
    type Item = Bytes;
    type Error = Error;
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Bytes>, Error> {
        if src.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(src[..4].try_into().expect("checked length")) as usize;
        if !(28..=DEFAULT_MAX_BODY + HEADER_LEN + 28).contains(&len) {
            return Err(Error::Length(len));
        }
        if src.len() < len + 4 {
            return Ok(None);
        }
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(&src[4..16]), &src[16..len + 4])
            .map_err(|_| Error::Crypto)?;
        let _ = src.split_to(len + 4);
        Ok(Some(Bytes::from(plaintext)))
    }
    fn decode_eof(&mut self, src: &mut BytesMut) -> Result<Option<Bytes>, Error> {
        match self.decode(src)? {
            Some(p) => Ok(Some(p)),
            None if src.is_empty() => Ok(None),
            None => Err(Error::Truncated),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nist_empty_plaintext_known_answer() {
        // NIST SP 800-38D, zero 128-bit key, zero 96-bit IV, empty plaintext/AAD.
        let mut wire = BytesMut::from(
            &[
                28, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x58, 0xe2, 0xfc, 0xce, 0xfa,
                0x7e, 0x30, 0x61, 0x36, 0x7f, 0x1d, 0x57, 0xa4, 0xe7, 0x45, 0x5a,
            ][..],
        );
        assert_eq!(
            GcmRecords::new(&[0; 16]).decode(&mut wire).unwrap(),
            Some(Bytes::new())
        );
    }
    #[test]
    fn fragmentation_concatenation_and_tag_tampering() {
        let mut codec = GcmRecords::new(&[42; 16]);
        let mut wire = BytesMut::new();
        codec
            .encode(Bytes::from_static(b"synthetic"), &mut wire)
            .unwrap();
        for split in 0..wire.len() {
            let mut input = BytesMut::from(&wire[..split]);
            assert!(codec.decode(&mut input).unwrap().is_none());
            input.extend_from_slice(&wire[split..]);
            input.extend_from_slice(&wire);
            for _ in 0..2 {
                assert_eq!(
                    codec.decode(&mut input).unwrap().unwrap(),
                    &b"synthetic"[..]
                );
            }
        }
        let last = wire.len() - 1;
        wire[last] ^= 1;
        assert!(matches!(codec.decode(&mut wire), Err(Error::Crypto)));
    }
}
