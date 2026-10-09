//! Protocol messages (docs/protocol-template-push.md) in two encodings with identical fields.
//! The first encoding is length-prefixed binary frames (`u32` little-endian length, then a
//! type byte and the fields, integers little-endian, byte strings `u32`-length-prefixed). The
//! second encoding is JSON lines (serde, byte strings as lowercase hex in wire byte order).

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};

use bytes::Bytes;
use hayai_crypto::zcash_primitives;
use hayai_wire::WtxId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zcash_primitives::transaction::TxId;

/// Byte string encoded as hex in JSON.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct HexBytes(pub Bytes);

impl From<Vec<u8>> for HexBytes {
    fn from(v: Vec<u8>) -> Self {
        Self(Bytes::from(v))
    }
}

impl Serialize for HexBytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for HexBytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(s)
            .map(|v| Self(Bytes::from(v)))
            .map_err(serde::de::Error::custom)
    }
}

/// 32-byte value encoded as 64 hex characters in wire byte order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Hash32(pub [u8; 32]);

impl Serialize for Hash32 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for Hash32 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(s).map_err(serde::de::Error::custom)?;
        <[u8; 32]>::try_from(v)
            .map(Self)
            .map_err(|v| serde::de::Error::custom(format!("expected 32 bytes, got {}", v.len())))
    }
}

/// `WtxId` encoded as 128 hex characters: txid then auth digest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WtxIdHex(pub WtxId);

impl Serialize for WtxIdHex {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(self.0.to_bytes()))
    }
}

impl<'de> Deserialize<'de> for WtxIdHex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(s).map_err(serde::de::Error::custom)?;
        let bytes = <[u8; 64]>::try_from(v)
            .map_err(|v| serde::de::Error::custom(format!("expected 64 bytes, got {}", v.len())))?;
        Ok(Self(wtxid_from_bytes(&bytes)))
    }
}

fn wtxid_from_bytes(bytes: &[u8; 64]) -> WtxId {
    let mut txid = [0u8; 32];
    txid.copy_from_slice(&bytes[..32]);
    let mut auth_digest = [0u8; 32];
    auth_digest.copy_from_slice(&bytes[32..]);
    WtxId {
        txid: TxId::from_bytes(txid),
        auth_digest,
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Subscribe {
    /// Script that the coinbase pays to.
    pub coinbase_target: HexBytes,
    pub max_block_bytes: u32,
    /// Whether template messages carry transaction wire bytes.
    pub want_full_txs: bool,
}

/// One template transaction. `bytes` is present when the subscriber asked for full
/// transactions.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TemplateTx {
    pub wtxid: WtxIdHex,
    pub len: u32,
    pub bytes: Option<HexBytes>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TemplateFull {
    pub template_id: u64,
    pub parent_hash: Hash32,
    pub height: u32,
    pub time: u32,
    pub bits: u32,
    pub version: u32,
    pub coinbase: HexBytes,
    pub txs: Vec<TemplateTx>,
    pub merkle_root: Hash32,
    pub auth_data_root: Hash32,
    pub block_commitments: Hash32,
    /// Coinbase expiry height.
    pub expiry: u32,
    pub fees_total: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AddedTx {
    /// Index in the new transaction list, after the removals.
    pub position: u32,
    pub wtxid: WtxIdHex,
    pub len: u32,
    pub bytes: Option<HexBytes>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TemplateDelta {
    pub template_id: u64,
    pub base_template_id: u64,
    /// Indexes into the base transaction list, ascending.
    pub removed: Vec<u32>,
    /// Insertions into the list after removals, ascending by position.
    pub added: Vec<AddedTx>,
    /// New coinbase when the fee total changed.
    pub coinbase: Option<HexBytes>,
    pub merkle_root: Hash32,
    pub auth_data_root: Hash32,
    pub block_commitments: Hash32,
    pub fees_total: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TemplateEmpty {
    pub template_id: u64,
    pub parent_hash: Hash32,
    pub height: u32,
    pub time: u32,
    pub bits: u32,
    pub version: u32,
    pub coinbase: HexBytes,
    pub merkle_root: Hash32,
    pub auth_data_root: Hash32,
    pub block_commitments: Hash32,
    pub expiry: u32,
}

/// The speculative block `rejected_hash` failed verification. `template` is the full
/// template on its parent again; the pool switches to it at once.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TemplateRevert {
    pub rejected_hash: Hash32,
    pub template: TemplateFull,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Submit {
    pub template_id: u64,
    pub time: u32,
    pub nonce: Hash32,
    pub solution: HexBytes,
    /// Replacement coinbase. The transaction set is the set of the template.
    pub coinbase: Option<HexBytes>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SubmitResult {
    pub template_id: u64,
    pub accepted: bool,
    pub reason: String,
    pub block_hash: Option<Hash32>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    Subscribe(Subscribe),
    TemplateFull(TemplateFull),
    TemplateDelta(TemplateDelta),
    TemplateEmpty(TemplateEmpty),
    TemplateRevert(TemplateRevert),
    Submit(Submit),
    SubmitResult(SubmitResult),
}

#[derive(thiserror::Error, Debug)]
pub enum CodecError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown message type {0}")]
    UnknownType(u8),
    #[error("frame of {0} bytes exceeds the limit")]
    FrameTooLarge(u32),
    #[error("trailing bytes in frame")]
    Trailing,
    #[error("invalid field: {0}")]
    Invalid(&'static str),
}

/// Largest frame that the decoder accepts: a full template with 2 MB of transactions plus
/// hex-free overhead.
pub const MAX_FRAME_BYTES: u32 = 4 * 1024 * 1024;

const TAG_SUBSCRIBE: u8 = 1;
const TAG_FULL: u8 = 2;
const TAG_DELTA: u8 = 3;
const TAG_EMPTY: u8 = 4;
const TAG_SUBMIT: u8 = 5;
const TAG_RESULT: u8 = 6;
const TAG_REVERT: u8 = 7;

impl Message {
    /// Serializes as one JSON line (with the trailing newline).
    pub fn to_json_line(&self) -> Result<Vec<u8>, CodecError> {
        let mut v = serde_json::to_vec(self)?;
        v.push(b'\n');
        Ok(v)
    }

    pub fn from_json_line(line: &[u8]) -> Result<Self, CodecError> {
        Ok(serde_json::from_slice(line)?)
    }

    /// Serializes as a binary frame: `u32` little-endian payload length, then the payload.
    pub fn to_frame(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(256);
        self.write_payload(&mut payload);
        let len = u32::try_from(payload.len()).expect("frame fits in u32");
        let mut frame = Vec::with_capacity(payload.len() + 4);
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&payload);
        frame
    }

    /// Reads one frame from a stream.
    pub fn read_frame(mut reader: impl Read) -> Result<Self, CodecError> {
        let mut len = [0u8; 4];
        reader.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len);
        if len > MAX_FRAME_BYTES {
            return Err(CodecError::FrameTooLarge(len));
        }
        let mut payload = vec![0u8; len as usize];
        reader.read_exact(&mut payload)?;
        Self::from_payload(&payload)
    }

    pub fn write_frame(&self, mut writer: impl Write) -> Result<(), CodecError> {
        writer.write_all(&self.to_frame())?;
        Ok(())
    }

    fn write_payload(&self, w: &mut Vec<u8>) {
        match self {
            Message::Subscribe(m) => {
                w.push(TAG_SUBSCRIBE);
                put_bytes(w, &m.coinbase_target.0);
                put_u32(w, m.max_block_bytes);
                w.push(u8::from(m.want_full_txs));
            }
            Message::TemplateFull(m) => {
                w.push(TAG_FULL);
                put_full(w, m);
            }
            Message::TemplateRevert(m) => {
                w.push(TAG_REVERT);
                w.extend_from_slice(&m.rejected_hash.0);
                put_full(w, &m.template);
            }
            Message::TemplateDelta(m) => {
                w.push(TAG_DELTA);
                put_u64(w, m.template_id);
                put_u64(w, m.base_template_id);
                put_u32(w, len_u32(m.removed.len()));
                for i in &m.removed {
                    put_u32(w, *i);
                }
                put_u32(w, len_u32(m.added.len()));
                for tx in &m.added {
                    put_u32(w, tx.position);
                    w.extend_from_slice(&tx.wtxid.0.to_bytes());
                    put_u32(w, tx.len);
                    put_opt_bytes(w, tx.bytes.as_ref());
                }
                put_opt_bytes(w, m.coinbase.as_ref());
                w.extend_from_slice(&m.merkle_root.0);
                w.extend_from_slice(&m.auth_data_root.0);
                w.extend_from_slice(&m.block_commitments.0);
                put_u64(w, m.fees_total);
            }
            Message::TemplateEmpty(m) => {
                w.push(TAG_EMPTY);
                put_u64(w, m.template_id);
                w.extend_from_slice(&m.parent_hash.0);
                put_u32(w, m.height);
                put_u32(w, m.time);
                put_u32(w, m.bits);
                put_u32(w, m.version);
                put_bytes(w, &m.coinbase.0);
                w.extend_from_slice(&m.merkle_root.0);
                w.extend_from_slice(&m.auth_data_root.0);
                w.extend_from_slice(&m.block_commitments.0);
                put_u32(w, m.expiry);
            }
            Message::Submit(m) => {
                w.push(TAG_SUBMIT);
                put_u64(w, m.template_id);
                put_u32(w, m.time);
                w.extend_from_slice(&m.nonce.0);
                put_bytes(w, &m.solution.0);
                put_opt_bytes(w, m.coinbase.as_ref());
            }
            Message::SubmitResult(m) => {
                w.push(TAG_RESULT);
                put_u64(w, m.template_id);
                w.push(u8::from(m.accepted));
                put_bytes(w, m.reason.as_bytes());
                match &m.block_hash {
                    Some(h) => {
                        w.push(1);
                        w.extend_from_slice(&h.0);
                    }
                    None => w.push(0),
                }
            }
        }
    }

    fn from_payload(payload: &[u8]) -> Result<Self, CodecError> {
        let mut r = Cursor { buf: payload };
        let tag = r.u8()?;
        let msg = match tag {
            TAG_SUBSCRIBE => Message::Subscribe(Subscribe {
                coinbase_target: r.bytes()?,
                max_block_bytes: r.u32()?,
                want_full_txs: r.bool()?,
            }),
            TAG_FULL => Message::TemplateFull(read_full(&mut r)?),
            TAG_REVERT => Message::TemplateRevert(TemplateRevert {
                rejected_hash: r.hash()?,
                template: read_full(&mut r)?,
            }),
            TAG_DELTA => {
                let template_id = r.u64()?;
                let base_template_id = r.u64()?;
                let count = r.u32()?;
                let mut removed = Vec::with_capacity(count.min(1 << 16) as usize);
                for _ in 0..count {
                    removed.push(r.u32()?);
                }
                let count = r.u32()?;
                let mut added = Vec::with_capacity(count.min(1 << 16) as usize);
                for _ in 0..count {
                    added.push(AddedTx {
                        position: r.u32()?,
                        wtxid: r.wtxid()?,
                        len: r.u32()?,
                        bytes: r.opt_bytes()?,
                    });
                }
                Message::TemplateDelta(TemplateDelta {
                    template_id,
                    base_template_id,
                    removed,
                    added,
                    coinbase: r.opt_bytes()?,
                    merkle_root: r.hash()?,
                    auth_data_root: r.hash()?,
                    block_commitments: r.hash()?,
                    fees_total: r.u64()?,
                })
            }
            TAG_EMPTY => Message::TemplateEmpty(TemplateEmpty {
                template_id: r.u64()?,
                parent_hash: r.hash()?,
                height: r.u32()?,
                time: r.u32()?,
                bits: r.u32()?,
                version: r.u32()?,
                coinbase: r.bytes()?,
                merkle_root: r.hash()?,
                auth_data_root: r.hash()?,
                block_commitments: r.hash()?,
                expiry: r.u32()?,
            }),
            TAG_SUBMIT => Message::Submit(Submit {
                template_id: r.u64()?,
                time: r.u32()?,
                nonce: r.hash()?,
                solution: r.bytes()?,
                coinbase: r.opt_bytes()?,
            }),
            TAG_RESULT => {
                let template_id = r.u64()?;
                let accepted = r.bool()?;
                let reason = String::from_utf8(r.bytes()?.0.to_vec())
                    .map_err(|_| CodecError::Invalid("reason is not UTF-8"))?;
                let block_hash = match r.u8()? {
                    0 => None,
                    1 => Some(r.hash()?),
                    _ => return Err(CodecError::Invalid("block_hash flag")),
                };
                Message::SubmitResult(SubmitResult {
                    template_id,
                    accepted,
                    reason,
                    block_hash,
                })
            }
            other => return Err(CodecError::UnknownType(other)),
        };
        if !r.buf.is_empty() {
            return Err(CodecError::Trailing);
        }
        Ok(msg)
    }
}

fn put_full(w: &mut Vec<u8>, m: &TemplateFull) {
    put_u64(w, m.template_id);
    w.extend_from_slice(&m.parent_hash.0);
    put_u32(w, m.height);
    put_u32(w, m.time);
    put_u32(w, m.bits);
    put_u32(w, m.version);
    put_bytes(w, &m.coinbase.0);
    put_u32(w, len_u32(m.txs.len()));
    for tx in &m.txs {
        w.extend_from_slice(&tx.wtxid.0.to_bytes());
        put_u32(w, tx.len);
        put_opt_bytes(w, tx.bytes.as_ref());
    }
    w.extend_from_slice(&m.merkle_root.0);
    w.extend_from_slice(&m.auth_data_root.0);
    w.extend_from_slice(&m.block_commitments.0);
    put_u32(w, m.expiry);
    put_u64(w, m.fees_total);
}

fn read_full(r: &mut Cursor<'_>) -> Result<TemplateFull, CodecError> {
    let template_id = r.u64()?;
    let parent_hash = r.hash()?;
    let height = r.u32()?;
    let time = r.u32()?;
    let bits = r.u32()?;
    let version = r.u32()?;
    let coinbase = r.bytes()?;
    let count = r.u32()?;
    let mut txs = Vec::with_capacity(count.min(1 << 16) as usize);
    for _ in 0..count {
        txs.push(TemplateTx {
            wtxid: r.wtxid()?,
            len: r.u32()?,
            bytes: r.opt_bytes()?,
        });
    }
    Ok(TemplateFull {
        template_id,
        parent_hash,
        height,
        time,
        bits,
        version,
        coinbase,
        txs,
        merkle_root: r.hash()?,
        auth_data_root: r.hash()?,
        block_commitments: r.hash()?,
        expiry: r.u32()?,
        fees_total: r.u64()?,
    })
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).expect("counts fit in u32")
}

fn put_u32(w: &mut Vec<u8>, v: u32) {
    w.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(w: &mut Vec<u8>, v: u64) {
    w.extend_from_slice(&v.to_le_bytes());
}

fn put_bytes(w: &mut Vec<u8>, b: &[u8]) {
    put_u32(w, len_u32(b.len()));
    w.extend_from_slice(b);
}

fn put_opt_bytes(w: &mut Vec<u8>, b: Option<&HexBytes>) {
    match b {
        Some(b) => {
            w.push(1);
            put_bytes(w, &b.0);
        }
        None => w.push(0),
    }
}

struct Cursor<'a> {
    buf: &'a [u8],
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], CodecError> {
        if self.buf.len() < n {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn bool(&mut self) -> Result<bool, CodecError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CodecError::Invalid("bool")),
        }
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn hash(&mut self) -> Result<Hash32, CodecError> {
        Ok(Hash32(self.take(32)?.try_into().expect("32 bytes")))
    }

    fn wtxid(&mut self) -> Result<WtxIdHex, CodecError> {
        let bytes: [u8; 64] = self.take(64)?.try_into().expect("64 bytes");
        Ok(WtxIdHex(wtxid_from_bytes(&bytes)))
    }

    fn bytes(&mut self) -> Result<HexBytes, CodecError> {
        let len = self.u32()? as usize;
        Ok(HexBytes(Bytes::copy_from_slice(self.take(len)?)))
    }

    fn opt_bytes(&mut self) -> Result<Option<HexBytes>, CodecError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.bytes()?)),
            _ => Err(CodecError::Invalid("option flag")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wtxid(b: u8) -> WtxIdHex {
        WtxIdHex(WtxId {
            txid: TxId::from_bytes([b; 32]),
            auth_digest: [b.wrapping_add(1); 32],
        })
    }

    fn samples() -> Vec<Message> {
        vec![
            Message::Subscribe(Subscribe {
                coinbase_target: vec![0x76, 0xa9].into(),
                max_block_bytes: 2_000_000,
                want_full_txs: true,
            }),
            Message::TemplateFull(TemplateFull {
                template_id: 7,
                parent_hash: Hash32([1; 32]),
                height: 100,
                time: 200,
                bits: 300,
                version: 4,
                coinbase: vec![9, 9, 9].into(),
                txs: vec![
                    TemplateTx {
                        wtxid: wtxid(2),
                        len: 3,
                        bytes: Some(vec![1, 2, 3].into()),
                    },
                    TemplateTx {
                        wtxid: wtxid(3),
                        len: 5,
                        bytes: None,
                    },
                ],
                merkle_root: Hash32([4; 32]),
                auth_data_root: Hash32([5; 32]),
                block_commitments: Hash32([6; 32]),
                expiry: 100,
                fees_total: 12345,
            }),
            Message::TemplateDelta(TemplateDelta {
                template_id: 8,
                base_template_id: 7,
                removed: vec![0, 5],
                added: vec![AddedTx {
                    position: 1,
                    wtxid: wtxid(9),
                    len: 2,
                    bytes: None,
                }],
                coinbase: Some(vec![8].into()),
                merkle_root: Hash32([4; 32]),
                auth_data_root: Hash32([5; 32]),
                block_commitments: Hash32([6; 32]),
                fees_total: 1,
            }),
            Message::TemplateEmpty(TemplateEmpty {
                template_id: 6,
                parent_hash: Hash32([1; 32]),
                height: 100,
                time: 200,
                bits: 300,
                version: 4,
                coinbase: vec![9].into(),
                merkle_root: Hash32([4; 32]),
                auth_data_root: Hash32([5; 32]),
                block_commitments: Hash32([6; 32]),
                expiry: 100,
            }),
            Message::TemplateRevert(TemplateRevert {
                rejected_hash: Hash32([0xee; 32]),
                template: TemplateFull {
                    template_id: 9,
                    parent_hash: Hash32([1; 32]),
                    height: 100,
                    time: 202,
                    bits: 300,
                    version: 4,
                    coinbase: vec![9, 9].into(),
                    txs: vec![TemplateTx {
                        wtxid: wtxid(4),
                        len: 3,
                        bytes: Some(vec![1, 2, 3].into()),
                    }],
                    merkle_root: Hash32([4; 32]),
                    auth_data_root: Hash32([5; 32]),
                    block_commitments: Hash32([6; 32]),
                    expiry: 100,
                    fees_total: 7,
                },
            }),
            Message::Submit(Submit {
                template_id: 7,
                time: 201,
                nonce: Hash32([7; 32]),
                solution: vec![0; 1344].into(),
                coinbase: None,
            }),
            Message::SubmitResult(SubmitResult {
                template_id: 7,
                accepted: false,
                reason: "stale".into(),
                block_hash: Some(Hash32([3; 32])),
            }),
        ]
    }

    #[test]
    fn binary_frames_round_trip() {
        for msg in samples() {
            let frame = msg.to_frame();
            let len = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
            assert_eq!(len, frame.len() - 4);
            let back = Message::read_frame(frame.as_slice()).unwrap();
            assert_eq!(back, msg);
        }
    }

    #[test]
    fn json_lines_round_trip_with_hex_fields() {
        for msg in samples() {
            let line = msg.to_json_line().unwrap();
            assert_eq!(*line.last().unwrap(), b'\n');
            let back = Message::from_json_line(&line).unwrap();
            assert_eq!(back, msg);
        }
        let line = samples()[1].to_json_line().unwrap();
        let text = String::from_utf8(line).unwrap();
        assert!(text.contains("\"type\":\"TemplateFull\""));
        assert!(text.contains(&format!("\"parent_hash\":\"{}\"", "01".repeat(32))));
        assert!(text.contains(&format!(
            "\"wtxid\":\"{}{}\"",
            "02".repeat(32),
            "03".repeat(32)
        )));
    }

    #[test]
    fn malformed_frames_are_rejected() {
        let mut frame = samples()[0].to_frame();
        frame.push(0);
        let len = (frame.len() - 4) as u32;
        frame[..4].copy_from_slice(&len.to_le_bytes());
        assert!(matches!(
            Message::read_frame(frame.as_slice()),
            Err(CodecError::Trailing)
        ));
        let frame = [1u8, 0, 0, 0, 99];
        assert!(matches!(
            Message::read_frame(&frame[..]),
            Err(CodecError::UnknownType(99))
        ));
        let frame = [0xffu8, 0xff, 0xff, 0xff];
        assert!(matches!(
            Message::read_frame(&frame[..]),
            Err(CodecError::FrameTooLarge(_))
        ));
    }
}
