//! The legacy Zcash peer-to-peer wire protocol: Bitcoin message framing and the message
//! payloads that zcashd, Zebra and Zakura exchange.
//!
//! A frame is `magic [4] || command [12, NUL padded] || length u32 LE || checksum [4] ||
//! payload`, the checksum being the first four bytes of SHA-256d of the payload. Payload
//! integers are little-endian except ports in network addresses (big-endian); counts are
//! canonical Bitcoin `CompactSize`.
//!
//! The compact-relay extension travels inside this framing as two commands: `zcmpctver`
//! (negotiation, see [`crate::protocol`]) and `zcmpct`, whose payload is one `hayai-relay`
//! frame. Everything else on the stream is exactly what a legacy node sends, so proxies and
//! legacy peers see one ordinary stream.
//!
//! Decoding never panics on any input and rejects a payload before allocating for it
//! when its declared counts cannot fit in the bytes that follow.

use std::io::{self, Cursor, Read};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use bytes::Bytes;
use hayai_crypto::zcash_encoding::CompactSize;
use hayai_crypto::zcash_protocol::TxId;
use hayai_wire::header::{BlockHash, BlockHeader, PowParams};
use hayai_wire::WtxId;
use sha2::{Digest, Sha256};

use crate::protocol::CompactVer;

/// Length of the frame header.
pub const FRAME_HEADER_LEN: usize = 24;
/// Largest payload of a legacy command (`MAX_PROTOCOL_MESSAGE_LEN` in zcashd and Zebra:
/// a 2,000,000-byte block fits with its framing). ZIP 204: at most 2,097,152 bytes.
pub const MAX_BODY_LEN: usize = 2 * 1024 * 1024;
/// Largest payload accepted before the handshake completes (Zebra uses the same bound).
pub const MAX_HANDSHAKE_BODY_LEN: usize = 1024;
/// Largest payload of a `zcmpct` command: one relay frame.
pub const MAX_COMPACT_BODY_LEN: usize = hayai_relay::MAX_PAYLOAD + 4;
/// Inventory entries per `inv`/`getdata`/`notfound` (zcashd `MAX_INV_SZ`). ZIP 204: at
/// most 50,000.
pub const MAX_INV_ENTRIES: usize = 50_000;
/// Entries per `addr` or `addrv2` message (zcashd `MAX_ADDR_TO_SEND`). ZIP 204, ZIP 155:
/// at most 1,000.
pub const MAX_ADDR_ENTRIES: usize = 1000;
/// Largest address field of an `addrv2` entry. ZIP 155: at most 512 bytes.
pub const MAX_ADDRV2_ADDR_LEN: usize = 512;
/// `addrv2` network id of an IPv4 address, 4 bytes (ZIP 155).
pub const ADDRV2_IPV4: u8 = 1;
/// `addrv2` network id of an IPv6 address, 16 bytes (ZIP 155).
pub const ADDRV2_IPV6: u8 = 2;
/// Headers per `headers` message (zcashd `MAX_HEADERS_RESULTS`). ZIP 204: at most 160.
pub const MAX_HEADERS: usize = 160;
/// Locator hashes per `getheaders` (zcashd `MAX_LOCATOR_SZ`).
pub const MAX_LOCATOR_HASHES: usize = 101;
/// User agent length (zcashd `MAX_SUBVERSION_LENGTH`). ZIP 204: at most 256 bytes.
pub const MAX_USER_AGENT_LEN: usize = 256;
/// `reject` field limits (zcashd `MAX_REJECT_MESSAGE_LENGTH`).
pub const MAX_REJECT_MESSAGE_LEN: usize = 12;
pub const MAX_REJECT_REASON_LEN: usize = 111;
/// `filterload` filter bytes (BIP 37 `MAX_BLOOM_FILTER_SIZE`) and `filteradd` data.
pub const MAX_FILTER_LEN: usize = 36_000;
/// ZIP 204: `filteradd` data has at most 520 bytes.
pub const MAX_FILTER_ADD_LEN: usize = 520;

/// Which Zcash network a stream belongs to; selects the frame magic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Network {
    Mainnet,
    Testnet,
    Regtest,
}

impl Network {
    /// The Equihash parameters of the network: the solution length of every header on it.
    pub fn pow(self) -> PowParams {
        match self {
            Network::Mainnet => PowParams::MAINNET,
            Network::Testnet => PowParams::TESTNET,
            Network::Regtest => PowParams::REGTEST,
        }
    }

    /// ZIP 204: the magic bytes of the network.
    pub fn magic(self) -> [u8; 4] {
        match self {
            Network::Mainnet => [0x24, 0xe9, 0x27, 0x64],
            Network::Testnet => [0xfa, 0x1a, 0xf9, 0xbf],
            Network::Regtest => [0xaa, 0xe8, 0x3f, 0x5f],
        }
    }
}

/// Inventory types of `inv`/`getdata`/`notfound`. `MSG_WTX` (ZIP 239) carries a 64-byte
/// `txid || auth_digest`; the others carry a 32-byte hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum InvItem {
    Error([u8; 32]),
    Tx(TxId),
    Block(BlockHash),
    FilteredBlock(BlockHash),
    Wtx(WtxId),
}

pub const MSG_ERROR: u32 = 0;
pub const MSG_TX: u32 = 1;
pub const MSG_BLOCK: u32 = 2;
pub const MSG_FILTERED_BLOCK: u32 = 3;
/// ZIP 239: the inventory type of a transaction by wtxid.
pub const MSG_WTX: u32 = 5;

/// A network address as carried in `version` (without timestamp) and `addr` messages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NetAddr {
    pub services: u64,
    pub addr: SocketAddr,
}

/// An `addr` entry: a [`NetAddr`] with the last-seen time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimedNetAddr {
    pub time: u32,
    pub net: NetAddr,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VersionMessage {
    pub version: u32,
    pub services: u64,
    pub timestamp: i64,
    pub addr_recv: NetAddr,
    pub addr_from: NetAddr,
    pub nonce: u64,
    pub user_agent: String,
    pub start_height: u32,
    /// Absent on the wire means `true` (BIP 37).
    pub relay: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reject {
    pub message: String,
    pub code: u8,
    pub reason: String,
    pub data: Option<[u8; 32]>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GetHeaders {
    pub version: u32,
    pub locator: Vec<BlockHash>,
    /// All-zero means "as many as fit".
    pub stop: BlockHash,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FilterLoad {
    pub filter: Bytes,
    pub hash_functions: u32,
    pub tweak: u32,
    pub flags: u8,
}

/// One legacy message. Variants marked decode-only are recognised so that the stream stays
/// in sync, and are answered with nothing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LegacyMessage {
    Version(VersionMessage),
    Verack,
    Ping(u64),
    Pong(u64),
    Addr(Vec<TimedNetAddr>),
    /// The ZIP 155 form of `addr`. The decoder keeps the IPv4 and IPv6 entries and drops the
    /// entries of every other network id.
    AddrV2(Vec<TimedNetAddr>),
    GetAddr,
    Inv(Vec<InvItem>),
    GetData(Vec<InvItem>),
    NotFound(Vec<InvItem>),
    /// Transaction wire bytes, unparsed.
    Tx(Bytes),
    /// Block wire bytes, unparsed.
    Block(Bytes),
    /// Full headers, each followed on the wire by a zero transaction count.
    Headers(Vec<BlockHeader>),
    GetHeaders(GetHeaders),
    /// The request for the block hashes after a locator. It has the fields of
    /// `getheaders`. Zebra and Zakura read the chain of a peer with it.
    GetBlocks(GetHeaders),
    Mempool,
    Reject(Reject),
    /// Decode-only.
    FilterLoad(FilterLoad),
    /// Decode-only.
    FilterAdd(Bytes),
    /// Decode-only.
    FilterClear,
    /// Decode-only (BIP 155 signalling). ZIP 155 has no such message: a Zcash peer sends
    /// `addrv2` without it.
    SendAddrV2,
    /// Compact-relay negotiation (`zcmpctver`).
    CompactVer(CompactVer),
    /// One compact-relay frame (`zcmpct`).
    Compact(hayai_relay::Message),
    /// A command this codec does not know, with an unparsed payload. Legacy nodes ignore
    /// these, and so does the session.
    Unknown {
        command: [u8; 12],
        payload: Bytes,
    },
}

pub const CMD_VERSION: &[u8; 12] = b"version\0\0\0\0\0";
pub const CMD_VERACK: &[u8; 12] = b"verack\0\0\0\0\0\0";
pub const CMD_PING: &[u8; 12] = b"ping\0\0\0\0\0\0\0\0";
pub const CMD_PONG: &[u8; 12] = b"pong\0\0\0\0\0\0\0\0";
pub const CMD_ADDR: &[u8; 12] = b"addr\0\0\0\0\0\0\0\0";
pub const CMD_ADDRV2: &[u8; 12] = b"addrv2\0\0\0\0\0\0";
pub const CMD_GETADDR: &[u8; 12] = b"getaddr\0\0\0\0\0";
pub const CMD_INV: &[u8; 12] = b"inv\0\0\0\0\0\0\0\0\0";
pub const CMD_GETDATA: &[u8; 12] = b"getdata\0\0\0\0\0";
pub const CMD_NOTFOUND: &[u8; 12] = b"notfound\0\0\0\0";
pub const CMD_TX: &[u8; 12] = b"tx\0\0\0\0\0\0\0\0\0\0";
pub const CMD_BLOCK: &[u8; 12] = b"block\0\0\0\0\0\0\0";
pub const CMD_HEADERS: &[u8; 12] = b"headers\0\0\0\0\0";
pub const CMD_GETHEADERS: &[u8; 12] = b"getheaders\0\0";
pub const CMD_GETBLOCKS: &[u8; 12] = b"getblocks\0\0\0";
pub const CMD_MEMPOOL: &[u8; 12] = b"mempool\0\0\0\0\0";
pub const CMD_REJECT: &[u8; 12] = b"reject\0\0\0\0\0\0";
pub const CMD_FILTERLOAD: &[u8; 12] = b"filterload\0\0";
pub const CMD_FILTERADD: &[u8; 12] = b"filteradd\0\0\0";
pub const CMD_FILTERCLEAR: &[u8; 12] = b"filterclear\0";
pub const CMD_SENDADDRV2: &[u8; 12] = b"sendaddrv2\0\0";
pub const CMD_ZCMPCTVER: &[u8; 12] = b"zcmpctver\0\0\0";
pub const CMD_ZCMPCT: &[u8; 12] = b"zcmpct\0\0\0\0\0\0";

impl LegacyMessage {
    pub fn command(&self) -> [u8; 12] {
        *match self {
            LegacyMessage::Version(_) => CMD_VERSION,
            LegacyMessage::Verack => CMD_VERACK,
            LegacyMessage::Ping(_) => CMD_PING,
            LegacyMessage::Pong(_) => CMD_PONG,
            LegacyMessage::Addr(_) => CMD_ADDR,
            LegacyMessage::AddrV2(_) => CMD_ADDRV2,
            LegacyMessage::GetAddr => CMD_GETADDR,
            LegacyMessage::Inv(_) => CMD_INV,
            LegacyMessage::GetData(_) => CMD_GETDATA,
            LegacyMessage::NotFound(_) => CMD_NOTFOUND,
            LegacyMessage::Tx(_) => CMD_TX,
            LegacyMessage::Block(_) => CMD_BLOCK,
            LegacyMessage::Headers(_) => CMD_HEADERS,
            LegacyMessage::GetHeaders(_) => CMD_GETHEADERS,
            LegacyMessage::GetBlocks(_) => CMD_GETBLOCKS,
            LegacyMessage::Mempool => CMD_MEMPOOL,
            LegacyMessage::Reject(_) => CMD_REJECT,
            LegacyMessage::FilterLoad(_) => CMD_FILTERLOAD,
            LegacyMessage::FilterAdd(_) => CMD_FILTERADD,
            LegacyMessage::FilterClear => CMD_FILTERCLEAR,
            LegacyMessage::SendAddrV2 => CMD_SENDADDRV2,
            LegacyMessage::CompactVer(_) => CMD_ZCMPCTVER,
            LegacyMessage::Compact(_) => CMD_ZCMPCT,
            LegacyMessage::Unknown { command, .. } => command,
        }
    }

    /// The command as printable text, for logs.
    pub fn command_name(&self) -> String {
        command_name(&self.command())
    }
}

pub fn command_name(command: &[u8; 12]) -> String {
    let end = command.iter().position(|b| *b == 0).unwrap_or(12);
    String::from_utf8_lossy(&command[..end]).into_owned()
}

/// Largest payload of a command whose fields have fixed lengths, or a `reject`. The
/// largest known layout is the `reject` message (139 bytes with its data). The bound
/// leaves room for fields that a newer protocol version adds.
pub const MAX_SMALL_BODY_LEN: usize = MAX_HANDSHAKE_BODY_LEN;
/// Bytes of the largest CompactSize.
const COUNT_LEN: usize = 9;

/// Largest payload that a command may carry on `network`. The reader checks the length
/// field of the frame header against it before it reads the payload, so a peer cannot
/// make the node hold 2 MB for a message whose layout is small.
///
/// - `zcmpct`: the relay frame bound.
/// - A command with a list: the largest count of the list times the largest entry.
/// - A command with fixed fields and `reject`: [`MAX_SMALL_BODY_LEN`].
/// - `tx`, `block` and a command that this decoder does not know: [`MAX_BODY_LEN`].
pub fn max_body_len(network: Network, command: &[u8; 12]) -> usize {
    const SMALL: [&[u8; 12]; 11] = [
        CMD_VERSION,
        CMD_VERACK,
        CMD_PING,
        CMD_PONG,
        CMD_GETADDR,
        CMD_MEMPOOL,
        CMD_REJECT,
        CMD_FILTERADD,
        CMD_FILTERCLEAR,
        CMD_SENDADDRV2,
        CMD_ZCMPCTVER,
    ];
    let listed = |entry: usize, count: usize| COUNT_LEN + entry * count;
    let max = if command == CMD_ZCMPCT {
        return MAX_COMPACT_BODY_LEN;
    } else if SMALL.contains(&command) {
        MAX_SMALL_BODY_LEN
    } else if command == CMD_ADDR {
        listed(30, MAX_ADDR_ENTRIES)
    } else if command == CMD_ADDRV2 {
        // Time, services (a CompactSize), network id, address length and address, port.
        listed(
            4 + COUNT_LEN + 1 + 3 + MAX_ADDRV2_ADDR_LEN + 2,
            MAX_ADDR_ENTRIES,
        )
    } else if command == CMD_INV || command == CMD_GETDATA || command == CMD_NOTFOUND {
        listed(36, MAX_INV_ENTRIES)
    } else if command == CMD_HEADERS {
        listed(network.pow().header_len() + 1, MAX_HEADERS)
    } else if command == CMD_GETHEADERS || command == CMD_GETBLOCKS {
        4 + listed(32, MAX_LOCATOR_HASHES) + 32
    } else if command == CMD_FILTERLOAD {
        MAX_FILTER_LEN + 9
    } else {
        MAX_BODY_LEN
    };
    max.min(MAX_BODY_LEN)
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum DecodeError {
    #[error("frame magic {0:02x?} is not this network's")]
    Magic([u8; 4]),
    #[error("{command}: payload of {len} bytes exceeds the {max} byte limit")]
    Oversize {
        command: String,
        len: usize,
        max: usize,
    },
    #[error("{0}: checksum mismatch")]
    Checksum(String),
    #[error("{0}: payload ended early")]
    Short(&'static str),
    #[error("{0}: invalid CompactSize")]
    CompactSize(&'static str),
    #[error("{field}: count {count} exceeds the limit of {max}")]
    CountLimit {
        field: &'static str,
        count: u64,
        max: usize,
    },
    #[error("{field}: count {count} needs more than the {remaining} remaining bytes")]
    CountTooLarge {
        field: &'static str,
        count: u64,
        remaining: usize,
    },
    #[error("{field}: {len} bytes exceeds the limit of {max}")]
    LengthLimit {
        field: &'static str,
        len: usize,
        max: usize,
    },
    #[error("{0}: not valid UTF-8")]
    Utf8(&'static str),
    #[error("addrv2: network id {network_id} with an address of {len} bytes")]
    AddrV2Length { network_id: u8, len: usize },
    #[error("inventory type {0} is unknown")]
    InvType(u32),
    #[error("headers entry {0} carries transactions")]
    HeadersWithTxs(usize),
    #[error("header: {0}")]
    Header(String),
    #[error("compact-relay frame: {0}")]
    Compact(#[from] hayai_relay::DecodeError),
    #[error("{0}: {1} bytes of trailing data")]
    Trailing(&'static str, usize),
}

/// The fixed part of a frame, parsed from its first [`FRAME_HEADER_LEN`] bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameHeader {
    pub command: [u8; 12],
    pub length: usize,
    pub checksum: [u8; 4],
}

impl FrameHeader {
    /// Parses a frame header, checking the magic and that the declared length is within
    /// `max_body` and the command's own limit.
    pub fn parse(
        network: Network,
        bytes: &[u8; FRAME_HEADER_LEN],
        max_body: usize,
    ) -> Result<Self, DecodeError> {
        let magic: [u8; 4] = bytes[..4].try_into().expect("4 bytes");
        // ZIP 204: refuse a frame with the magic of another network.
        if magic != network.magic() {
            return Err(DecodeError::Magic(magic));
        }
        let command: [u8; 12] = bytes[4..16].try_into().expect("12 bytes");
        let length = u32::from_le_bytes(bytes[16..20].try_into().expect("4 bytes")) as usize;
        let max = max_body.min(max_body_len(network, &command));
        // ZIP 204: refuse a length above the limit, before the payload is read.
        if length > max {
            return Err(DecodeError::Oversize {
                command: command_name(&command),
                len: length,
                max,
            });
        }
        let checksum: [u8; 4] = bytes[20..24].try_into().expect("4 bytes");
        Ok(FrameHeader {
            command,
            length,
            checksum,
        })
    }
}

pub fn checksum(payload: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(payload);
    let second = Sha256::digest(first);
    second[..4].try_into().expect("4 bytes")
}

/// Encodes a message as one frame.
pub fn encode(network: Network, message: &LegacyMessage) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + 64);
    out.extend_from_slice(&network.magic());
    out.extend_from_slice(&message.command());
    out.extend_from_slice(&[0u8; 8]);
    encode_body(message, &mut out);
    let body_len = out.len() - FRAME_HEADER_LEN;
    let len = u32::try_from(body_len).expect("payload length fits u32");
    out[16..20].copy_from_slice(&len.to_le_bytes());
    let sum = checksum(&out[FRAME_HEADER_LEN..]);
    out[20..24].copy_from_slice(&sum);
    out
}

/// Encodes a message payload without framing.
pub fn encode_body(message: &LegacyMessage, out: &mut Vec<u8>) {
    let mut w = Writer(out);
    match message {
        LegacyMessage::Version(v) => {
            w.u32(v.version);
            w.u64(v.services);
            w.bytes(&v.timestamp.to_le_bytes());
            w.net_addr(&v.addr_recv);
            w.net_addr(&v.addr_from);
            w.u64(v.nonce);
            w.var_str(&v.user_agent);
            w.u32(v.start_height);
            // ZIP 204: the version message has the relay field.
            w.u8(u8::from(v.relay));
        }
        LegacyMessage::Verack
        | LegacyMessage::GetAddr
        | LegacyMessage::Mempool
        | LegacyMessage::FilterClear
        | LegacyMessage::SendAddrV2 => {}
        LegacyMessage::Ping(nonce) | LegacyMessage::Pong(nonce) => w.u64(*nonce),
        LegacyMessage::Addr(addrs) => {
            w.compact_size(addrs.len());
            for a in addrs {
                w.u32(a.time);
                w.net_addr(&a.net);
            }
        }
        LegacyMessage::AddrV2(addrs) => {
            w.compact_size(addrs.len());
            for a in addrs {
                w.u32(a.time);
                w.compact_u64(a.net.services);
                match a.net.addr.ip() {
                    IpAddr::V4(v4) => {
                        w.u8(ADDRV2_IPV4);
                        w.compact_size(4);
                        w.bytes(&v4.octets());
                    }
                    IpAddr::V6(v6) => {
                        w.u8(ADDRV2_IPV6);
                        w.compact_size(16);
                        w.bytes(&v6.octets());
                    }
                }
                w.bytes(&a.net.addr.port().to_be_bytes());
            }
        }
        LegacyMessage::Inv(items)
        | LegacyMessage::GetData(items)
        | LegacyMessage::NotFound(items) => {
            w.compact_size(items.len());
            for item in items {
                match item {
                    InvItem::Error(h) => {
                        w.u32(MSG_ERROR);
                        w.bytes(h);
                    }
                    InvItem::Tx(id) => {
                        w.u32(MSG_TX);
                        w.bytes(id.as_ref());
                    }
                    InvItem::Block(h) => {
                        w.u32(MSG_BLOCK);
                        w.bytes(&h.0);
                    }
                    InvItem::FilteredBlock(h) => {
                        w.u32(MSG_FILTERED_BLOCK);
                        w.bytes(&h.0);
                    }
                    InvItem::Wtx(id) => {
                        w.u32(MSG_WTX);
                        w.bytes(&id.to_bytes());
                    }
                }
            }
        }
        LegacyMessage::Tx(bytes)
        | LegacyMessage::Block(bytes)
        | LegacyMessage::FilterAdd(bytes) => {
            w.bytes(bytes);
        }
        LegacyMessage::Headers(headers) => {
            w.compact_size(headers.len());
            for h in headers {
                w.bytes(&h.serialize());
                w.compact_size(0);
            }
        }
        LegacyMessage::GetHeaders(g) | LegacyMessage::GetBlocks(g) => {
            w.u32(g.version);
            w.compact_size(g.locator.len());
            for h in &g.locator {
                w.bytes(&h.0);
            }
            w.bytes(&g.stop.0);
        }
        LegacyMessage::Reject(r) => {
            w.var_str(&r.message);
            w.u8(r.code);
            w.var_str(&r.reason);
            if let Some(data) = &r.data {
                w.bytes(data);
            }
        }
        LegacyMessage::FilterLoad(f) => {
            w.bytes(&f.filter);
            w.u32(f.hash_functions);
            w.u32(f.tweak);
            w.u8(f.flags);
        }
        LegacyMessage::CompactVer(v) => {
            w.bytes(&v.max_version.to_le_bytes());
            w.bytes(&v.min_version.to_le_bytes());
            w.u64(v.features);
        }
        LegacyMessage::Compact(m) => w.bytes(&hayai_relay::encode(m)),
        LegacyMessage::Unknown { payload, .. } => w.bytes(payload),
    }
}

/// Decodes one complete frame; the slice must hold exactly one frame.
pub fn decode(network: Network, frame: &[u8]) -> Result<LegacyMessage, DecodeError> {
    let Some(head) = frame.get(..FRAME_HEADER_LEN) else {
        return Err(DecodeError::Short("frame header"));
    };
    let header = FrameHeader::parse(network, head.try_into().expect("24 bytes"), usize::MAX)?;
    let body = &frame[FRAME_HEADER_LEN..];
    if body.len() < header.length {
        return Err(DecodeError::Short("frame payload"));
    }
    if body.len() > header.length {
        return Err(DecodeError::Trailing("frame", body.len() - header.length));
    }
    // ZIP 204: refuse a payload whose checksum does not match.
    if checksum(body) != header.checksum {
        return Err(DecodeError::Checksum(command_name(&header.command)));
    }
    decode_body(network, &header.command, body)
}

/// Decodes a payload whose frame header was already checked.
///
/// Bitcoin nodes accept extra bytes at the end of most payloads so that newer formats stay
/// readable by older nodes; this decoder does too, except for the payloads whose length is
/// their only delimiter (`tx`, `block`, `filteradd`, `zcmpct`).
///
/// `network` sets the solution length of the headers in a `headers` message.
pub fn decode_body(
    network: Network,
    command: &[u8; 12],
    body: &[u8],
) -> Result<LegacyMessage, DecodeError> {
    let mut r = Reader(Cursor::new(body));
    let message = match command {
        CMD_VERSION => LegacyMessage::Version(VersionMessage {
            version: r.u32("version")?,
            services: r.u64("services")?,
            timestamp: i64::from_le_bytes(r.array("timestamp")?),
            addr_recv: r.net_addr("addr_recv")?,
            addr_from: r.net_addr("addr_from")?,
            nonce: r.u64("nonce")?,
            user_agent: r.var_str("user_agent", MAX_USER_AGENT_LEN)?,
            start_height: r.u32("start_height")?,
            // ZIP 204: an absent relay field is true. The node takes each byte other than
            // 0 as true, as zcashd, and differs from the SHOULD to refuse a value above 1.
            relay: if r.remaining() == 0 {
                true
            } else {
                r.u8("relay")? != 0
            },
        }),
        CMD_VERACK => LegacyMessage::Verack,
        CMD_PING => LegacyMessage::Ping(r.u64("nonce")?),
        CMD_PONG => LegacyMessage::Pong(r.u64("nonce")?),
        CMD_ADDR => {
            let count = r.count("addrs", 30, MAX_ADDR_ENTRIES)?;
            let mut addrs = Vec::with_capacity(count);
            for _ in 0..count {
                addrs.push(TimedNetAddr {
                    time: r.u32("addr time")?,
                    net: r.net_addr("addr")?,
                });
            }
            LegacyMessage::Addr(addrs)
        }
        CMD_ADDRV2 => {
            // Smallest entry: time, services, network id, an empty address, port.
            let count = r.count("addrv2", 4 + 1 + 1 + 1 + 2, MAX_ADDR_ENTRIES)?;
            let mut addrs = Vec::with_capacity(count);
            for _ in 0..count {
                if let Some(entry) = r.addr_v2()? {
                    addrs.push(entry);
                }
            }
            LegacyMessage::AddrV2(addrs)
        }
        CMD_GETADDR => LegacyMessage::GetAddr,
        CMD_INV => LegacyMessage::Inv(r.inv("inv")?),
        CMD_GETDATA => LegacyMessage::GetData(r.inv("getdata")?),
        CMD_NOTFOUND => LegacyMessage::NotFound(r.inv("notfound")?),
        CMD_TX => LegacyMessage::Tx(Bytes::copy_from_slice(body)),
        CMD_BLOCK => LegacyMessage::Block(Bytes::copy_from_slice(body)),
        CMD_HEADERS => {
            let pow = network.pow();
            let count = r.count("headers", pow.header_len() + 1, MAX_HEADERS)?;
            let mut headers = Vec::with_capacity(count);
            for i in 0..count {
                let header = r.header(pow)?;
                // ZIP 204: the transaction count after each header is 0.
                if r.compact_size("header tx count")? != 0 {
                    return Err(DecodeError::HeadersWithTxs(i));
                }
                headers.push(header);
            }
            LegacyMessage::Headers(headers)
        }
        CMD_GETHEADERS | CMD_GETBLOCKS => {
            let version = r.u32("version")?;
            let count = r.count("locator", 32, MAX_LOCATOR_HASHES)?;
            let mut locator = Vec::with_capacity(count);
            for _ in 0..count {
                locator.push(BlockHash(r.array("locator hash")?));
            }
            let request = GetHeaders {
                version,
                locator,
                stop: BlockHash(r.array("stop hash")?),
            };
            if command == CMD_GETBLOCKS {
                LegacyMessage::GetBlocks(request)
            } else {
                LegacyMessage::GetHeaders(request)
            }
        }
        CMD_MEMPOOL => LegacyMessage::Mempool,
        CMD_REJECT => {
            let message = r.var_str("reject message", MAX_REJECT_MESSAGE_LEN)?;
            let code = r.u8("reject code")?;
            let reason = r.var_str("reject reason", MAX_REJECT_REASON_LEN)?;
            let data = if r.remaining() >= 32 {
                Some(r.array("reject data")?)
            } else {
                None
            };
            LegacyMessage::Reject(Reject {
                message,
                code,
                reason,
                data,
            })
        }
        CMD_FILTERLOAD => {
            const FIXED: usize = 4 + 4 + 1;
            if body.len() < FIXED || body.len() > MAX_FILTER_LEN + FIXED {
                return Err(DecodeError::LengthLimit {
                    field: "filterload",
                    len: body.len(),
                    max: MAX_FILTER_LEN + FIXED,
                });
            }
            let filter = r.take("filter", body.len() - FIXED)?;
            LegacyMessage::FilterLoad(FilterLoad {
                filter,
                hash_functions: r.u32("hash_functions")?,
                tweak: r.u32("tweak")?,
                flags: r.u8("flags")?,
            })
        }
        CMD_FILTERADD => {
            if body.len() > MAX_FILTER_ADD_LEN {
                return Err(DecodeError::LengthLimit {
                    field: "filteradd",
                    len: body.len(),
                    max: MAX_FILTER_ADD_LEN,
                });
            }
            LegacyMessage::FilterAdd(Bytes::copy_from_slice(body))
        }
        CMD_FILTERCLEAR => LegacyMessage::FilterClear,
        CMD_SENDADDRV2 => LegacyMessage::SendAddrV2,
        CMD_ZCMPCTVER => LegacyMessage::CompactVer(CompactVer {
            max_version: u16::from_le_bytes(r.array("max_version")?),
            min_version: u16::from_le_bytes(r.array("min_version")?),
            features: r.u64("features")?,
        }),
        CMD_ZCMPCT => LegacyMessage::Compact(hayai_relay::decode(body)?),
        // ZIP 204: a command that is not one of the known commands (also one with bytes
        // that are not printable, or not NUL after the first NUL) is ignored.
        other => LegacyMessage::Unknown {
            command: *other,
            payload: Bytes::copy_from_slice(body),
        },
    };
    Ok(message)
}

/// Reads one frame from a blocking reader. `max_body` bounds the payload on top of the
/// command's own limit (the handshake uses [`MAX_HANDSHAKE_BODY_LEN`]).
pub fn read_message(
    reader: &mut impl Read,
    network: Network,
    max_body: usize,
) -> Result<LegacyMessage, ReadError> {
    let mut head = [0u8; FRAME_HEADER_LEN];
    reader.read_exact(&mut head)?;
    let header = FrameHeader::parse(network, &head, max_body)?;
    let mut body = vec![0u8; header.length];
    reader.read_exact(&mut body)?;
    // ZIP 204: refuse a payload whose checksum does not match.
    if checksum(&body) != header.checksum {
        return Err(DecodeError::Checksum(command_name(&header.command)).into());
    }
    Ok(decode_body(network, &header.command, &body)?)
}

#[derive(thiserror::Error, Debug)]
pub enum ReadError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("decode: {0}")]
    Decode(#[from] DecodeError),
}

struct Writer<'a>(&'a mut Vec<u8>);

impl Writer<'_> {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn compact_size(&mut self, n: usize) {
        CompactSize::write(&mut *self.0, n).expect("writing to a Vec cannot fail");
    }
    /// A CompactSize over the full `u64` range (the service bits of `addrv2`).
    fn compact_u64(&mut self, n: u64) {
        match n {
            0..=0xfc => self.u8(n as u8),
            0xfd..=0xffff => {
                self.u8(0xfd);
                self.bytes(&(n as u16).to_le_bytes());
            }
            0x1_0000..=0xffff_ffff => {
                self.u8(0xfe);
                self.u32(n as u32);
            }
            _ => {
                self.u8(0xff);
                self.u64(n);
            }
        }
    }
    fn var_str(&mut self, s: &str) {
        self.compact_size(s.len());
        self.bytes(s.as_bytes());
    }
    /// Services, IPv6 (IPv4 mapped), big-endian port.
    fn net_addr(&mut self, a: &NetAddr) {
        self.u64(a.services);
        let ip = match a.addr.ip() {
            IpAddr::V4(v4) => v4.to_ipv6_mapped(),
            IpAddr::V6(v6) => v6,
        };
        self.bytes(&ip.octets());
        self.bytes(&a.addr.port().to_be_bytes());
    }
}

struct Reader<'a>(Cursor<&'a [u8]>);

impl Reader<'_> {
    fn remaining(&self) -> usize {
        self.0
            .get_ref()
            .len()
            .saturating_sub(self.0.position() as usize)
    }
    /// A header of the network whose Equihash parameters are `pow`, delimited by its own
    /// solution length prefix.
    fn header(&mut self, pow: PowParams) -> Result<BlockHeader, DecodeError> {
        let start = self.0.position() as usize;
        let header = BlockHeader::parse(&self.0.get_ref()[start..])
            .map_err(|e| DecodeError::Header(e.to_string()))?;
        if header.solution.len() != pow.solution_len() {
            return Err(DecodeError::Header(format!(
                "solution of {} bytes, the network needs {}",
                header.solution.len(),
                pow.solution_len()
            )));
        }
        self.0
            .set_position((start + header.serialized_len()) as u64);
        Ok(header)
    }
    fn array<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], DecodeError> {
        let mut out = [0u8; N];
        self.0
            .read_exact(&mut out)
            .map_err(|_| DecodeError::Short(field))?;
        Ok(out)
    }
    fn u8(&mut self, field: &'static str) -> Result<u8, DecodeError> {
        Ok(self.array::<1>(field)?[0])
    }
    fn u32(&mut self, field: &'static str) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array(field)?))
    }
    fn u64(&mut self, field: &'static str) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.array(field)?))
    }
    /// ZIP 204: refuse a CompactSize that is not canonical (upstream `CompactSize::read`).
    fn compact_size(&mut self, field: &'static str) -> Result<u64, DecodeError> {
        CompactSize::read(&mut self.0).map_err(|_| DecodeError::CompactSize(field))
    }
    /// A canonical CompactSize over the full `u64` range. `CompactSize::read` stops at
    /// 0x02000000, which is below the service bits of `addrv2`.
    fn compact_u64(&mut self, field: &'static str) -> Result<u64, DecodeError> {
        let (value, min) = match self.u8(field)? {
            0xfd => (u64::from(u16::from_le_bytes(self.array(field)?)), 0xfd),
            0xfe => (u64::from(self.u32(field)?), 0x1_0000),
            0xff => (self.u64(field)?, 0x1_0000_0000),
            small => (u64::from(small), 0),
        };
        // ZIP 204: refuse a CompactSize that is not canonical.
        if value < min {
            return Err(DecodeError::CompactSize(field));
        }
        Ok(value)
    }
    /// One `addrv2` entry (ZIP 155). `None` for a network id other than IPv4 and IPv6: the
    /// entry is consumed and dropped. An IPv4 or IPv6 entry with another length is an error.
    fn addr_v2(&mut self) -> Result<Option<TimedNetAddr>, DecodeError> {
        let time = self.u32("addrv2 time")?;
        let services = self.compact_u64("addrv2 services")?;
        let network_id = self.u8("addrv2 network id")?;
        let len = self.compact_u64("addrv2 address length")?;
        // ZIP 155: refuse an addr field of more than 512 bytes, whatever the network id.
        if len > MAX_ADDRV2_ADDR_LEN as u64 {
            return Err(DecodeError::LengthLimit {
                field: "addrv2 address",
                len: usize::try_from(len).unwrap_or(usize::MAX),
                max: MAX_ADDRV2_ADDR_LEN,
            });
        }
        let len = len as usize;
        let bytes = self.take("addrv2 address", len)?;
        let port = u16::from_be_bytes(self.array("addrv2 port")?);
        let ip = match (network_id, len) {
            (ADDRV2_IPV4, 4) => IpAddr::from(<[u8; 4]>::try_from(&bytes[..]).expect("4 bytes")),
            (ADDRV2_IPV6, 16) => {
                let v6 = Ipv6Addr::from(<[u8; 16]>::try_from(&bytes[..]).expect("16 bytes"));
                match v6.to_ipv4_mapped() {
                    Some(v4) => IpAddr::V4(v4),
                    None => IpAddr::V6(v6),
                }
            }
            // ZIP 155: refuse a known network id with another address length. The node
            // checks IPv4 and IPv6 only; a TORV3, I2P or CJDNS entry of another length is
            // dropped, not refused, as in Zakura.
            (ADDRV2_IPV4 | ADDRV2_IPV6, _) => {
                return Err(DecodeError::AddrV2Length { network_id, len });
            }
            // ZIP 155: an entry of another network is not gossiped (also id 0x03, Tor v2).
            _ => return Ok(None),
        };
        Ok(Some(TimedNetAddr {
            time,
            net: NetAddr {
                services,
                addr: SocketAddr::new(ip, port),
            },
        }))
    }
    /// A count of items of at least `min_item` bytes each, bounded by `max` and by the
    /// remaining input, so that no allocation is made for a count the payload cannot hold.
    fn count(
        &mut self,
        field: &'static str,
        min_item: usize,
        max: usize,
    ) -> Result<usize, DecodeError> {
        let count = self.compact_size(field)?;
        if count > max as u64 {
            return Err(DecodeError::CountLimit { field, count, max });
        }
        let remaining = self.remaining();
        if count * min_item as u64 > remaining as u64 {
            return Err(DecodeError::CountTooLarge {
                field,
                count,
                remaining,
            });
        }
        Ok(count as usize)
    }
    fn take(&mut self, field: &'static str, len: usize) -> Result<Bytes, DecodeError> {
        if len > self.remaining() {
            return Err(DecodeError::Short(field));
        }
        let start = self.0.position() as usize;
        let bytes = Bytes::copy_from_slice(&self.0.get_ref()[start..start + len]);
        self.0.set_position((start + len) as u64);
        Ok(bytes)
    }
    fn var_str(&mut self, field: &'static str, max: usize) -> Result<String, DecodeError> {
        let len = self.compact_size(field)?;
        if len > max as u64 {
            return Err(DecodeError::LengthLimit {
                field,
                len: len as usize,
                max,
            });
        }
        let bytes = self.take(field, len as usize)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::Utf8(field))
    }
    fn net_addr(&mut self, field: &'static str) -> Result<NetAddr, DecodeError> {
        let services = self.u64(field)?;
        let ip = Ipv6Addr::from(self.array::<16>(field)?);
        let port = u16::from_be_bytes(self.array(field)?);
        let ip = match ip.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(ip),
        };
        Ok(NetAddr {
            services,
            addr: SocketAddr::new(ip, port),
        })
    }
    fn inv(&mut self, field: &'static str) -> Result<Vec<InvItem>, DecodeError> {
        let count = self.count(field, 36, MAX_INV_ENTRIES)?;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = self.u32("inv type")?;
            items.push(match kind {
                MSG_ERROR => InvItem::Error(self.array("inv hash")?),
                MSG_TX => InvItem::Tx(TxId::from_bytes(self.array("inv hash")?)),
                MSG_BLOCK => InvItem::Block(BlockHash(self.array("inv hash")?)),
                MSG_FILTERED_BLOCK => InvItem::FilteredBlock(BlockHash(self.array("inv hash")?)),
                MSG_WTX => InvItem::Wtx(WtxId {
                    txid: TxId::from_bytes(self.array("inv txid")?),
                    auth_digest: self.array("inv auth digest")?,
                }),
                // ZIP 204, ZIP 239: refuse an unknown inventory type. Type 0 (MSG_ERROR) is
                // not in the ZIP 204 table; the node takes it, as Zakura.
                other => return Err(DecodeError::InvType(other)),
            });
        }
        Ok(items)
    }
}
