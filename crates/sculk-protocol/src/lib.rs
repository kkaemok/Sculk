//! Historical LOCO framing, independently implemented from public wire descriptions.
//!
//! This codec is for plaintext **inside** a negotiated secure transport, or local
//! test streams. It does not establish an authenticated KakaoTalk connection.
use bson::{
    raw::{RawBsonRef, RawDocument},
    Document,
};
use bytes::{BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// Header including the little-endian BSON body length.
pub const HEADER_LEN: usize = 22;
/// Default maximum body size, independent of peer-provided lengths.
pub const DEFAULT_MAX_BODY: usize = 1024 * 1024;
/// Maximum accepted nested BSON container depth.
pub const MAX_DEPTH: usize = 32;

/// Framing or serialization failure. Error messages never include payload data.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid command name or nonzero bytes after its first NUL.
    #[error("invalid command name")]
    Command,
    /// The payload type has not been implemented.
    #[error("unsupported payload type {0}")]
    PayloadType(u8),
    /// Header length exceeds the configured bound or cannot contain BSON.
    #[error("invalid or oversized body length {0}")]
    Length(usize),
    /// Invalid BSON, including excessive nesting.
    #[error("invalid BSON payload")]
    Bson,
    /// EOF occurred in a partial frame.
    #[error("truncated frame")]
    Truncated,
    /// Encryption or authentication-tag failure.
    #[error("secure record authentication failed")]
    Crypto,
    /// Stream I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Nonempty, printable ASCII command occupying at most eleven bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Command([u8; 11]);
impl Command {
    /// Validate a command without restricting it to a known command catalogue.
    pub fn new(name: &str) -> Result<Self, Error> {
        if name.is_empty() || name.len() > 11 || !name.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(Error::Command);
        }
        let mut bytes = [0; 11];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Self(bytes))
    }
    fn from_wire(bytes: &[u8]) -> Result<Self, Error> {
        let len = bytes.iter().position(|b| *b == 0).unwrap_or(11);
        if bytes[len..].iter().any(|b| *b != 0) {
            return Err(Error::Command);
        }
        Self::new(std::str::from_utf8(&bytes[..len]).map_err(|_| Error::Command)?)
    }
    /// Command name without padding.
    pub fn as_str(&self) -> &str {
        // Construction enforces ASCII, including when decoding.
        std::str::from_utf8(&self.0[..self.0.iter().position(|b| *b == 0).unwrap_or(11)])
            .expect("validated ASCII")
    }
}

/// A validated BSON packet. Payload bytes are reference counted and never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Packet {
    /// Request correlation identifier; zero is reserved by Sculk for unsolicited data.
    pub id: u32,
    /// Raw status bits (interpretation is command specific).
    pub status: u16,
    /// Command name; unknown names are preserved.
    pub command: Command,
    body: Bytes,
}
impl std::fmt::Debug for Packet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Packet")
            .field("id", &self.id)
            .field("status", &self.status)
            .field("command", &self.command)
            .field("body_len", &self.body.len())
            .finish()
    }
}
impl Packet {
    /// Serialize and validate a document, subject to the default body limit.
    pub fn from_document(id: u32, command: Command, document: &Document) -> Result<Self, Error> {
        let body = bson::to_vec(document).map_err(|_| Error::Bson)?;
        Self::from_bytes(id, 0, command, Bytes::from(body))
    }
    /// Accept BSON bytes subject to the default body and nesting limits.
    pub fn from_bytes(id: u32, status: u16, command: Command, body: Bytes) -> Result<Self, Error> {
        check_length(body.len(), DEFAULT_MAX_BODY)?;
        validate(&body)?;
        Ok(Self {
            id,
            status,
            command,
            body,
        })
    }
    /// Validated BSON without copying.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Materialize an owned BSON document only when needed.
    pub fn document(&self) -> Result<Document, Error> {
        bson::from_slice(&self.body).map_err(|_| Error::Bson)
    }
}
fn check_length(len: usize, max: usize) -> Result<(), Error> {
    if len < 5 || len > max || len > u32::MAX as usize {
        Err(Error::Length(len))
    } else {
        Ok(())
    }
}
fn validate(body: &[u8]) -> Result<(), Error> {
    fn value(v: RawBsonRef<'_>, depth: usize) -> Result<(), Error> {
        match v {
            RawBsonRef::Document(doc) => document(doc, depth + 1),
            RawBsonRef::Array(array) => {
                if depth >= MAX_DEPTH {
                    return Err(Error::Bson);
                }
                for item in array {
                    value(item.map_err(|_| Error::Bson)?, depth + 1)?;
                }
                Ok(())
            }
            RawBsonRef::JavaScriptCodeWithScope(code) => document(code.scope, depth + 1),
            _ => Ok(()),
        }
    }
    fn document(doc: &RawDocument, depth: usize) -> Result<(), Error> {
        if depth > MAX_DEPTH {
            return Err(Error::Bson);
        }
        for item in doc.iter() {
            let (_, v) = item.map_err(|_| Error::Bson)?;
            value(v, depth)?;
        }
        Ok(())
    }
    document(RawDocument::from_bytes(body).map_err(|_| Error::Bson)?, 0)
}

/// Incremental codec. An invalid frame is fatal: callers must close the stream.
/// It never reserves memory from an untrusted length before enforcing its limit.
#[derive(Debug, Clone)]
pub struct LocoCodec {
    max_body: usize,
}
impl Default for LocoCodec {
    fn default() -> Self {
        Self {
            max_body: DEFAULT_MAX_BODY,
        }
    }
}
impl LocoCodec {
    /// Set a smaller deployment-specific maximum (5 bytes through 1 MiB).
    pub fn new(max_body: usize) -> Result<Self, Error> {
        check_length(max_body, DEFAULT_MAX_BODY)?;
        Ok(Self { max_body })
    }
}
impl Decoder for LocoCodec {
    type Item = Packet;
    type Error = Error;
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Packet>, Error> {
        if src.len() < HEADER_LEN {
            return Ok(None);
        }
        let len = u32::from_le_bytes(src[18..22].try_into().expect("header checked")) as usize;
        check_length(len, self.max_body)?;
        let command = Command::from_wire(&src[6..17])?;
        if src[17] != 0 {
            return Err(Error::PayloadType(src[17]));
        }
        if src.len() < HEADER_LEN + len {
            return Ok(None);
        }
        validate(&src[HEADER_LEN..HEADER_LEN + len])?;
        let id = u32::from_le_bytes(src[..4].try_into().expect("header checked"));
        let status = u16::from_le_bytes(src[4..6].try_into().expect("header checked"));
        let frame = src.split_to(HEADER_LEN + len).freeze();
        Ok(Some(Packet {
            id,
            status,
            command,
            body: frame.slice(HEADER_LEN..),
        }))
    }
    fn decode_eof(&mut self, src: &mut BytesMut) -> Result<Option<Packet>, Error> {
        match self.decode(src)? {
            Some(p) => Ok(Some(p)),
            None if src.is_empty() => Ok(None),
            None => Err(Error::Truncated),
        }
    }
}
impl Encoder<Packet> for LocoCodec {
    type Error = Error;
    fn encode(&mut self, packet: Packet, dst: &mut BytesMut) -> Result<(), Error> {
        check_length(packet.body.len(), self.max_body)?;
        dst.reserve(HEADER_LEN + packet.body.len());
        dst.put_u32_le(packet.id);
        dst.put_u16_le(packet.status);
        dst.extend_from_slice(&packet.command.0);
        dst.put_u8(0); // BSON
        dst.put_u32_le(packet.body.len() as u32);
        dst.extend_from_slice(&packet.body);
        Ok(())
    }
}

/// Experimental AES-GCM record framing; no handshake or peer authentication.
#[cfg(feature = "experimental-gcm")]
pub mod secure;

/// Public-key-only LOCO key exchange, separately gated from record encryption.
#[cfg(feature = "loco-handshake")]
pub mod handshake;
