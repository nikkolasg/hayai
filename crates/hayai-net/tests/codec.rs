//! Legacy codec: byte-exact vectors (Zebra's and hand-built), round trips, limits and
//! garbage input.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use bytes::Bytes;
use hayai_crypto::zcash_protocol::TxId;
use hayai_net::codec::{
    checksum, decode, decode_body, encode, read_message, DecodeError, FilterLoad, GetHeaders,
    InvItem, LegacyMessage, NetAddr, Network, ReadError, Reject, TimedNetAddr, VersionMessage,
    CMD_ADDR, CMD_HEADERS, CMD_INV, CMD_VERSION, FRAME_HEADER_LEN, MAX_BODY_LEN,
    MAX_HANDSHAKE_BODY_LEN, MAX_INV_ENTRIES, MSG_WTX,
};
use hayai_net::protocol::CompactVer;
use hayai_wire::header::{BlockHash, BlockHeader, PowParams};
use hayai_wire::WtxId;
use proptest::prelude::*;

fn zebra_version() -> VersionMessage {
    let addr = NetAddr {
        services: 1,
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6)), 8233),
    };
    VersionMessage {
        version: 170_150,
        services: 1,
        timestamp: 1_568_000_000,
        addr_recv: addr,
        addr_from: addr,
        nonce: 0x9082_4908_8927_9238,
        user_agent: "Zebra".into(),
        start_height: 540_000,
        relay: true,
    }
}

#[test]
fn version_frame_matches_the_bitcoin_layout() {
    let frame = encode(Network::Mainnet, &LegacyMessage::Version(zebra_version()));
    let mut expected = Vec::new();
    expected.extend_from_slice(&[0x24, 0xe9, 0x27, 0x64]);
    expected.extend_from_slice(b"version\0\0\0\0\0");
    expected.extend_from_slice(&91u32.to_le_bytes());
    expected.extend_from_slice(&[0; 4]);
    let body_start = expected.len();
    expected.extend_from_slice(&170_150u32.to_le_bytes());
    expected.extend_from_slice(&1u64.to_le_bytes());
    expected.extend_from_slice(&1_568_000_000i64.to_le_bytes());
    for _ in 0..2 {
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(&hex::decode("00000000000000000000ffffcb007106").unwrap());
        expected.extend_from_slice(&[0x20, 0x29]);
    }
    expected.extend_from_slice(&0x9082_4908_8927_9238u64.to_le_bytes());
    expected.extend_from_slice(b"\x05Zebra");
    expected.extend_from_slice(&540_000u32.to_le_bytes());
    expected.push(1);
    let sum = checksum(&expected[body_start..]);
    expected[20..24].copy_from_slice(&sum);
    assert_eq!(frame, expected);
    // Zebra's codec tests patch the timestamp at bytes 36..44 of the frame.
    assert_eq!(
        &frame[36..44],
        &1_568_000_000i64.to_le_bytes(),
        "timestamp offset"
    );
    assert_eq!(
        decode(Network::Mainnet, &frame),
        Ok(LegacyMessage::Version(zebra_version()))
    );
}

#[test]
fn version_relay_byte_is_optional_and_lenient() {
    let mut body = Vec::new();
    hayai_net::codec::encode_body(&LegacyMessage::Version(zebra_version()), &mut body);
    body.pop();
    let LegacyMessage::Version(v) = decode_body(Network::Mainnet, CMD_VERSION, &body).unwrap()
    else {
        panic!("version");
    };
    assert!(v.relay, "omitted relay means true");
    body.push(0);
    let LegacyMessage::Version(v) = decode_body(Network::Mainnet, CMD_VERSION, &body).unwrap()
    else {
        panic!("version");
    };
    assert!(!v.relay);
    // zcashd reads the field as a byte that is true when non-zero.
    body.pop();
    body.push(2);
    let LegacyMessage::Version(v) = decode_body(Network::Mainnet, CMD_VERSION, &body).unwrap()
    else {
        panic!("version");
    };
    assert!(v.relay);
    // Trailing fields of a future version format are tolerated.
    body.extend_from_slice(&[9, 9, 9]);
    assert!(matches!(
        decode_body(Network::Mainnet, CMD_VERSION, &body),
        Ok(LegacyMessage::Version(_))
    ));
}

#[test]
fn user_agent_limit() {
    let mut v = zebra_version();
    v.user_agent = "x".repeat(257);
    let mut body = Vec::new();
    hayai_net::codec::encode_body(&LegacyMessage::Version(v), &mut body);
    assert_eq!(
        decode_body(Network::Mainnet, CMD_VERSION, &body),
        Err(DecodeError::LengthLimit {
            field: "user_agent",
            len: 257,
            max: 256
        })
    );
}

/// Zebra's `ADDR_V1_IP_VECTORS` (zebra-test/src/network_addr.rs).
#[test]
fn zebra_addr_v1_vectors() {
    let two = hex::decode(concat!(
        "02",
        "61bc6649",
        "0000000000000000",
        "00000000000000000000000000000001",
        "0000",
        "79627683",
        "0100000000000000",
        "00000000000000000000000000000001",
        "00f1",
    ))
    .unwrap();
    let LegacyMessage::Addr(addrs) = decode_body(Network::Mainnet, CMD_ADDR, &two).unwrap() else {
        panic!("addr");
    };
    assert_eq!(
        addrs,
        vec![
            TimedNetAddr {
                time: 0x4966_bc61,
                net: NetAddr {
                    services: 0,
                    addr: "[::1]:0".parse().unwrap()
                }
            },
            TimedNetAddr {
                time: 0x8376_6279,
                net: NetAddr {
                    services: 1,
                    addr: "[::1]:241".parse().unwrap()
                }
            },
        ]
    );
    let mut re = Vec::new();
    hayai_net::codec::encode_body(&LegacyMessage::Addr(addrs), &mut re);
    assert_eq!(re, two);

    let mapped = hex::decode(concat!(
        "01",
        "61bc6649",
        "0100000000000000",
        "00000000000000000000ffff",
        "7f000001",
        "0000",
    ))
    .unwrap();
    let LegacyMessage::Addr(addrs) = decode_body(Network::Mainnet, CMD_ADDR, &mapped).unwrap()
    else {
        panic!("addr");
    };
    assert_eq!(addrs[0].net.addr, "127.0.0.1:0".parse().unwrap());
    let mut re = Vec::new();
    hayai_net::codec::encode_body(&LegacyMessage::Addr(addrs), &mut re);
    assert_eq!(re, mapped);

    let empty = hex::decode("00").unwrap();
    assert_eq!(
        decode_body(Network::Mainnet, CMD_ADDR, &empty),
        Ok(LegacyMessage::Addr(vec![]))
    );
}

/// Zebra's `parses_msg_wtx_inventory_type`: code 5 followed by 64 bytes.
#[test]
fn zebra_msg_wtx_vector() {
    let mut body = vec![1u8];
    body.extend_from_slice(&MSG_WTX.to_le_bytes());
    body.extend_from_slice(&[0u8; 64]);
    assert_eq!(
        decode_body(Network::Mainnet, CMD_INV, &body),
        Ok(LegacyMessage::Inv(vec![InvItem::Wtx(WtxId {
            txid: TxId::from_bytes([0; 32]),
            auth_digest: [0; 32],
        })]))
    );
    body[1] = 4;
    assert_eq!(
        decode_body(Network::Mainnet, CMD_INV, &body),
        Err(DecodeError::InvType(4))
    );
}

#[test]
fn inv_limits_apply_before_allocation() {
    // Count 50,001 with no bytes behind it.
    let body = [0xfd, 0x51, 0xc3];
    assert_eq!(
        decode_body(Network::Mainnet, CMD_INV, &body),
        Err(DecodeError::CountLimit {
            field: "inv",
            count: MAX_INV_ENTRIES as u64 + 1,
            max: MAX_INV_ENTRIES
        })
    );
    // Count 1000 but only one entry of bytes.
    let mut body = vec![0xfd, 0xe8, 0x03];
    body.extend_from_slice(&[0u8; 36]);
    assert_eq!(
        decode_body(Network::Mainnet, CMD_INV, &body),
        Err(DecodeError::CountTooLarge {
            field: "inv",
            count: 1000,
            remaining: 36
        })
    );
    // Non-canonical count.
    let body = [0xfd, 0x01, 0x00, 2, 0, 0, 0];
    assert_eq!(
        decode_body(Network::Mainnet, CMD_INV, &body),
        Err(DecodeError::CompactSize("inv"))
    );
}

#[test]
fn frame_header_checks() {
    let frame = encode(Network::Mainnet, &LegacyMessage::Verack);
    assert_eq!(
        decode(Network::Testnet, &frame),
        Err(DecodeError::Magic([0x24, 0xe9, 0x27, 0x64]))
    );
    assert_eq!(
        encode(Network::Testnet, &LegacyMessage::Verack)[..4],
        [0xfa, 0x1a, 0xf9, 0xbf]
    );
    assert_eq!(
        encode(Network::Regtest, &LegacyMessage::Verack)[..4],
        [0xaa, 0xe8, 0x3f, 0x5f]
    );

    let mut bad = encode(Network::Mainnet, &LegacyMessage::Ping(7));
    bad[FRAME_HEADER_LEN] ^= 1;
    assert_eq!(
        decode(Network::Mainnet, &bad),
        Err(DecodeError::Checksum("ping".into()))
    );

    let mut oversize = encode(Network::Mainnet, &LegacyMessage::Tx(Bytes::new()));
    oversize[16..20].copy_from_slice(&((MAX_BODY_LEN + 1) as u32).to_le_bytes());
    assert!(matches!(
        decode(Network::Mainnet, &oversize),
        Err(DecodeError::Oversize { .. })
    ));
    // Each command has its own bound, checked on the frame header: a `ping` with the
    // length of a block is refused before the payload is read.
    for (message, max) in [
        (LegacyMessage::Ping(1), 1024),
        (LegacyMessage::Verack, 1024),
        (LegacyMessage::GetAddr, 1024),
        (LegacyMessage::Addr(Vec::new()), 9 + 30 * 1000),
        (LegacyMessage::Inv(Vec::new()), 9 + 36 * 50_000),
        (LegacyMessage::Headers(Vec::new()), 9 + 1488 * 160),
    ] {
        let mut frame = encode(Network::Mainnet, &message);
        frame.truncate(FRAME_HEADER_LEN);
        frame[16..20].copy_from_slice(&(max as u32).to_le_bytes());
        let head: [u8; FRAME_HEADER_LEN] = frame[..].try_into().unwrap();
        assert_eq!(
            hayai_net::codec::FrameHeader::parse(Network::Mainnet, &head, usize::MAX)
                .map(|header| header.length),
            Ok(max),
            "{}",
            message.command_name()
        );
        frame[16..20].copy_from_slice(&(max as u32 + 1).to_le_bytes());
        let head: [u8; FRAME_HEADER_LEN] = frame[..].try_into().unwrap();
        assert_eq!(
            hayai_net::codec::FrameHeader::parse(Network::Mainnet, &head, usize::MAX),
            Err(DecodeError::Oversize {
                command: message.command_name(),
                len: max + 1,
                max,
            }),
        );
    }
    // A Regtest header is shorter, and so is the bound of `headers`.
    assert_eq!(
        hayai_net::codec::max_body_len(Network::Regtest, hayai_net::codec::CMD_HEADERS),
        9 + 178 * 160
    );

    // The handshake bound applies through `read_message`.
    let big = encode(
        Network::Mainnet,
        &LegacyMessage::Tx(Bytes::from(vec![0u8; 2000])),
    );
    let mut cursor = std::io::Cursor::new(&big);
    assert!(matches!(
        read_message(&mut cursor, Network::Mainnet, MAX_HANDSHAKE_BODY_LEN),
        Err(ReadError::Decode(DecodeError::Oversize { len: 2000, .. }))
    ));
    let mut cursor = std::io::Cursor::new(&big);
    assert!(matches!(
        read_message(&mut cursor, Network::Mainnet, usize::MAX),
        Ok(LegacyMessage::Tx(_))
    ));
    // A truncated stream is an I/O error, never a panic.
    let mut cursor = std::io::Cursor::new(&big[..big.len() - 1]);
    assert!(matches!(
        read_message(&mut cursor, Network::Mainnet, usize::MAX),
        Err(ReadError::Io(_))
    ));
}

#[test]
fn unknown_commands_pass_through() {
    let frame = encode(
        Network::Mainnet,
        &LegacyMessage::Unknown {
            command: *b"wtxidrelay\0\0",
            payload: Bytes::from_static(b"xyz"),
        },
    );
    assert_eq!(
        decode(Network::Mainnet, &frame),
        Ok(LegacyMessage::Unknown {
            command: *b"wtxidrelay\0\0",
            payload: Bytes::from_static(b"xyz"),
        })
    );
}

fn addr_v2(hex_body: &str) -> Result<Vec<TimedNetAddr>, DecodeError> {
    let body = hex::decode(hex_body).expect("hex");
    match decode_body(Network::Mainnet, hayai_net::codec::CMD_ADDRV2, &body)? {
        LegacyMessage::AddrV2(addrs) => Ok(addrs),
        other => panic!("expected addrv2, got {other:?}"),
    }
}

fn v2_entry(time: u32, services: u64, addr: &str) -> TimedNetAddr {
    TimedNetAddr {
        time,
        net: NetAddr {
            services,
            addr: addr.parse().expect("socket address"),
        },
    }
}

/// The ZIP 155 / BIP 155 vectors of zcashd `netbase_tests.cpp` (`stream_addrv2_hex`) and the
/// extra cases of Zebra, from `zebra-test/src/network_addr.rs` (`ADDR_V2_IP_VECTORS`,
/// `ADDR_V2_EMPTY_VECTORS`).
#[test]
fn addrv2_vectors_keep_ipv4_and_ipv6_and_drop_other_networks() {
    const TIME_2009: u32 = 0x4966_bc61;
    const TIME_2039: u32 = 0x8376_6279;
    const IPV6_ENTRY: &str = "796276830102100000000000000000000000000000000100f1";
    let ipv6_entry = v2_entry(TIME_2039, 1, "[::1]:241");
    // stream_addrv2_hex: two IPv6 entries and one Tor v3 entry.
    let got = addr_v2(concat!(
        "03",
        "61bc6649",
        "00",
        "02",
        "10",
        "00000000000000000000000000000001",
        "0000",
        "79627683",
        "01",
        "02",
        "10",
        "00000000000000000000000000000001",
        "00f1",
        "79627683",
        "01",
        "04",
        "20",
        "53cd5648488c4707914182655b7664034e09e66f7e8cbf1084e654eb56c5bd88",
        "235a",
    ))
    .unwrap();
    assert_eq!(got, vec![v2_entry(TIME_2009, 0, "[::1]:0"), ipv6_entry]);
    // IPv4, then IPv6.
    let got = addr_v2(&format!("02{}{IPV6_ENTRY}", "796276830101047f0000010001")).unwrap();
    assert_eq!(got, vec![v2_entry(TIME_2039, 1, "127.0.0.1:1"), ipv6_entry]);
    // Every service bit set: a nine-byte CompactSize.
    let got = addr_v2(concat!(
        "01",
        "79627683",
        "ffffffffffffffffff",
        "02",
        "10",
        "00000000000000000000000000000001",
        "0000",
    ))
    .unwrap();
    assert_eq!(got, vec![v2_entry(TIME_2039, u64::MAX, "[::1]:0")]);
    // Unknown network ids with an address of 8, 0 and 512 bytes: consumed and dropped.
    for unknown in [
        format!("7962768301fb08{}0001", "00".repeat(8)),
        "7962768301fc000001".to_string(),
        format!("7962768301fdfd0002{}0001", "00".repeat(512)),
    ] {
        let got = addr_v2(&format!("02{unknown}{IPV6_ENTRY}")).unwrap();
        assert_eq!(got, vec![ipv6_entry]);
    }
    // torv3_hex alone, and the empty list.
    let tor = concat!(
        "01",
        "79627683",
        "01",
        "04",
        "20",
        "53cd5648488c4707914182655b7664034e09e66f7e8cbf1084e654eb56c5bd88",
        "235a",
    );
    assert_eq!(addr_v2(tor), Ok(vec![]));
    assert_eq!(addr_v2("00"), Ok(vec![]));
}

/// The invalid vectors of Zebra (`ADDR_V2_INVALID_VECTORS`) and the length rules of
/// BIP 155 for the known network ids.
#[test]
fn addrv2_rejects_invalid_lengths() {
    const IPV6_ENTRY: &str = "796276830102100000000000000000000000000000000100f1";
    // An address of 513 bytes.
    let too_long = format!("027962768301fefd0102{}0001{IPV6_ENTRY}", "00".repeat(513));
    assert_eq!(
        addr_v2(&too_long),
        Err(DecodeError::LengthLimit {
            field: "addrv2 address",
            len: 513,
            max: 512
        })
    );
    // A length of 2^31 - 1 without the bytes.
    assert!(matches!(
        addr_v2("017962768301fffeffffff7f"),
        Err(DecodeError::LengthLimit { .. })
    ));
    // IPv4 with 5 bytes, IPv6 with 4 bytes (Bitcoin Core `net_tests.cpp`, "BIP155 IPv4
    // address with length 5 (should be 4)").
    assert_eq!(
        addr_v2("01796276830101050102030405208d"),
        Err(DecodeError::AddrV2Length {
            network_id: 1,
            len: 5
        })
    );
    assert_eq!(
        addr_v2("0179627683010204010203040001"),
        Err(DecodeError::AddrV2Length {
            network_id: 2,
            len: 4
        })
    );
    // A CompactSize that is not canonical, in the services and in the length.
    assert_eq!(
        addr_v2("0179627683fd0100010401020304208d"),
        Err(DecodeError::CompactSize("addrv2 services"))
    );
    assert_eq!(
        addr_v2("01796276830101fd040001020304208d"),
        Err(DecodeError::CompactSize("addrv2 address length"))
    );
    // More than 1,000 entries, and a count that the payload cannot hold.
    assert!(matches!(
        addr_v2("fde903"),
        Err(DecodeError::CountLimit { max: 1000, .. })
    ));
    assert!(matches!(
        addr_v2("05"),
        Err(DecodeError::CountTooLarge { .. })
    ));
    // An entry cut short.
    assert!(matches!(
        addr_v2("017962768301010401020304"),
        Err(DecodeError::Short(_))
    ));
}

#[test]
fn addrv2_encodes_ipv4_and_ipv6() {
    let addrs = vec![
        v2_entry(1, 1, "1.2.3.4:8333"),
        v2_entry(2, 0x0448, "[1a1b:2a2b:3a3b:4a4b:5a5b:6a6b:7a7b:8a8b]:8233"),
    ];
    let mut body = Vec::new();
    hayai_net::codec::encode_body(&LegacyMessage::AddrV2(addrs.clone()), &mut body);
    // The address forms of BIP 155: `01 04 01020304` and `02 10` with 16 bytes.
    assert_eq!(
        hex::encode(&body),
        concat!(
            "02",
            "01000000",
            "01",
            "01",
            "04",
            "01020304",
            "208d",
            "02000000",
            "fd4804",
            "02",
            "10",
            "1a1b2a2b3a3b4a4b5a5b6a6b7a7b8a8b",
            "2029",
        )
    );
    assert_eq!(addr_v2(&hex::encode(&body)), Ok(addrs));
}

fn header(n: u8) -> BlockHeader {
    header_of(n, PowParams::MAINNET)
}

fn header_of(n: u8, pow: PowParams) -> BlockHeader {
    BlockHeader {
        version: 4,
        prev_hash: BlockHash([n; 32]),
        merkle_root: [n.wrapping_add(1); 32],
        block_commitments: [2; 32],
        time: 1_700_000_000,
        bits: 0x1f07_ffff,
        nonce: [3; 32],
        solution: vec![n; pow.solution_len()],
    }
}

#[test]
fn headers_carry_full_headers_and_a_zero_tx_count() {
    let msg = LegacyMessage::Headers(vec![header(1), header(2)]);
    let mut body = Vec::new();
    hayai_net::codec::encode_body(&msg, &mut body);
    assert_eq!(body.len(), 1 + 2 * (1487 + 1));
    assert_eq!(body[0], 2);
    assert_eq!(&body[1..1488], &header(1).serialize()[..]);
    assert_eq!(body[1488], 0);
    assert_eq!(decode_body(Network::Mainnet, CMD_HEADERS, &body), Ok(msg));
    body[1488] = 1;
    assert_eq!(
        decode_body(Network::Mainnet, CMD_HEADERS, &body),
        Err(DecodeError::HeadersWithTxs(0))
    );
    // 161 headers are over the protocol cap.
    let body = [0xa1];
    assert!(matches!(
        decode_body(Network::Mainnet, CMD_HEADERS, &body),
        Err(DecodeError::CountLimit { max: 160, .. })
    ));
}

/// A Regtest `headers` message carries 177-byte headers. Each network accepts only its
/// own solution length.
#[test]
fn headers_follow_the_network_solution_length() {
    let regtest = LegacyMessage::Headers(vec![
        header_of(1, PowParams::REGTEST),
        header_of(2, PowParams::REGTEST),
    ]);
    let frame = encode(Network::Regtest, &regtest);
    assert_eq!(frame.len(), FRAME_HEADER_LEN + 1 + 2 * (177 + 1));
    assert_eq!(decode(Network::Regtest, &frame), Ok(regtest.clone()));
    let mut body = Vec::new();
    hayai_net::codec::encode_body(&regtest, &mut body);
    // The count bound uses the network's header length: two Mainnet headers need more
    // bytes than two Regtest headers occupy.
    for network in [Network::Mainnet, Network::Testnet] {
        let Err(DecodeError::CountTooLarge { .. }) = decode_body(network, CMD_HEADERS, &body)
        else {
            panic!("Regtest headers accepted on {network:?}");
        };
    }
    // Enough bytes for the count bound: the header itself is rejected.
    body.resize(1 + 2 * (1487 + 1), 0);
    let Err(DecodeError::Header(_)) = decode_body(Network::Mainnet, CMD_HEADERS, &body) else {
        panic!("a Regtest header accepted on Mainnet");
    };
    let mainnet = LegacyMessage::Headers(vec![header(1)]);
    let mut body = Vec::new();
    hayai_net::codec::encode_body(&mainnet, &mut body);
    let Err(DecodeError::Header(_)) = decode_body(Network::Regtest, CMD_HEADERS, &body) else {
        panic!("a Mainnet header accepted on Regtest");
    };
}

#[test]
fn filter_messages_decode_within_bounds() {
    let load = LegacyMessage::FilterLoad(FilterLoad {
        filter: Bytes::from(vec![0xab; 10]),
        hash_functions: 3,
        tweak: 9,
        flags: 1,
    });
    let frame = encode(Network::Mainnet, &load);
    assert_eq!(decode(Network::Mainnet, &frame), Ok(load));
    let too_big = LegacyMessage::FilterLoad(FilterLoad {
        filter: Bytes::from(vec![0; 36_001]),
        hash_functions: 0,
        tweak: 0,
        flags: 0,
    });
    // The bound of the command refuses the frame before the payload is read.
    assert!(matches!(
        decode(Network::Mainnet, &encode(Network::Mainnet, &too_big)),
        Err(DecodeError::Oversize {
            len: 36_010,
            max: 36_009,
            ..
        })
    ));
    let add = LegacyMessage::FilterAdd(Bytes::from(vec![1; 521]));
    assert!(matches!(
        decode(Network::Mainnet, &encode(Network::Mainnet, &add)),
        Err(DecodeError::LengthLimit { .. })
    ));
}

#[test]
fn zcmpctver_layout() {
    let msg = LegacyMessage::CompactVer(CompactVer {
        max_version: 2,
        min_version: 1,
        features: 3,
    });
    let frame = encode(Network::Mainnet, &msg);
    assert_eq!(&frame[4..16], b"zcmpctver\0\0\0");
    assert_eq!(
        &frame[FRAME_HEADER_LEN..],
        &[2, 0, 1, 0, 3, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(decode(Network::Mainnet, &frame), Ok(msg));
}

#[test]
fn zcmpct_wraps_one_relay_frame() {
    let inner = hayai_relay::Message::TxRequest(hayai_relay::TxRequest { ids: vec![] });
    let frame = encode(Network::Mainnet, &LegacyMessage::Compact(inner.clone()));
    assert_eq!(&frame[4..16], b"zcmpct\0\0\0\0\0\0");
    assert_eq!(&frame[FRAME_HEADER_LEN..], &hayai_relay::encode(&inner)[..]);
    assert_eq!(
        decode(Network::Mainnet, &frame),
        Ok(LegacyMessage::Compact(inner))
    );
    // A broken inner frame is a decode error (the session disconnects).
    let mut broken = frame.clone();
    let last = broken.len() - 1;
    broken[last] ^= 0xff;
    let sum = checksum(&broken[FRAME_HEADER_LEN..]);
    broken[20..24].copy_from_slice(&sum);
    assert!(matches!(
        decode(Network::Mainnet, &broken),
        Err(DecodeError::Compact(_))
    ));
}

fn sock_addr() -> impl Strategy<Value = SocketAddr> + Clone {
    prop_oneof![
        (any::<[u8; 4]>(), any::<u16>())
            .prop_map(|(ip, port)| SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port)),
        (any::<[u8; 16]>(), any::<u16>()).prop_filter_map("ipv4-mapped", |(ip, port)| {
            let ip = Ipv6Addr::from(ip);
            match ip.to_ipv4_mapped() {
                Some(_) => None,
                None => Some(SocketAddr::new(IpAddr::V6(ip), port)),
            }
        }),
    ]
}

fn net_addr() -> impl Strategy<Value = NetAddr> + Clone {
    (any::<u64>(), sock_addr()).prop_map(|(services, addr)| NetAddr { services, addr })
}

fn inv_item() -> impl Strategy<Value = InvItem> + Clone {
    prop_oneof![
        any::<[u8; 32]>().prop_map(InvItem::Error),
        any::<[u8; 32]>().prop_map(|h| InvItem::Tx(TxId::from_bytes(h))),
        any::<[u8; 32]>().prop_map(|h| InvItem::Block(BlockHash(h))),
        any::<[u8; 32]>().prop_map(|h| InvItem::FilteredBlock(BlockHash(h))),
        (any::<[u8; 32]>(), any::<[u8; 32]>()).prop_map(|(t, a)| InvItem::Wtx(WtxId {
            txid: TxId::from_bytes(t),
            auth_digest: a
        })),
    ]
}

fn ascii(max: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(0x20u8..0x7f, 0..=max).prop_map(|b| String::from_utf8(b).unwrap())
}

fn message() -> impl Strategy<Value = LegacyMessage> {
    let bytes = proptest::collection::vec(any::<u8>(), 0..200).prop_map(Bytes::from);
    let items = proptest::collection::vec(inv_item(), 0..8);
    prop_oneof![
        (
            any::<u32>(),
            any::<u64>(),
            any::<i64>(),
            net_addr(),
            net_addr(),
            any::<u64>(),
            ascii(40),
            any::<u32>(),
            any::<bool>()
        )
            .prop_map(
                |(version, services, timestamp, recv, from, nonce, ua, height, relay)| {
                    LegacyMessage::Version(VersionMessage {
                        version,
                        services,
                        timestamp,
                        addr_recv: recv,
                        addr_from: from,
                        nonce,
                        user_agent: ua,
                        start_height: height,
                        relay,
                    })
                }
            ),
        Just(LegacyMessage::Verack),
        any::<u64>().prop_map(LegacyMessage::Ping),
        any::<u64>().prop_map(LegacyMessage::Pong),
        proptest::collection::vec((any::<u32>(), net_addr()), 0..5).prop_map(|v| {
            LegacyMessage::Addr(
                v.into_iter()
                    .map(|(time, net)| TimedNetAddr { time, net })
                    .collect(),
            )
        }),
        proptest::collection::vec((any::<u32>(), net_addr()), 0..5).prop_map(|v| {
            LegacyMessage::AddrV2(
                v.into_iter()
                    .map(|(time, net)| TimedNetAddr { time, net })
                    .collect(),
            )
        }),
        Just(LegacyMessage::GetAddr),
        items.clone().prop_map(LegacyMessage::Inv),
        items.clone().prop_map(LegacyMessage::GetData),
        items.prop_map(LegacyMessage::NotFound),
        bytes.clone().prop_map(LegacyMessage::Tx),
        bytes.clone().prop_map(LegacyMessage::Block),
        proptest::collection::vec(any::<u8>(), 0..4)
            .prop_map(|v| LegacyMessage::Headers(v.into_iter().map(header).collect())),
        (
            any::<u32>(),
            proptest::collection::vec(any::<[u8; 32]>(), 0..5),
            any::<[u8; 32]>()
        )
            .prop_map(
                |(version, locator, stop)| LegacyMessage::GetHeaders(GetHeaders {
                    version,
                    locator: locator.into_iter().map(BlockHash).collect(),
                    stop: BlockHash(stop),
                })
            ),
        (
            any::<u32>(),
            proptest::collection::vec(any::<[u8; 32]>(), 0..5),
            any::<[u8; 32]>()
        )
            .prop_map(
                |(version, locator, stop)| LegacyMessage::GetBlocks(GetHeaders {
                    version,
                    locator: locator.into_iter().map(BlockHash).collect(),
                    stop: BlockHash(stop),
                })
            ),
        Just(LegacyMessage::Mempool),
        (
            ascii(12),
            any::<u8>(),
            ascii(111),
            proptest::option::of(any::<[u8; 32]>())
        )
            .prop_map(
                |(message, code, reason, data)| LegacyMessage::Reject(Reject {
                    message,
                    code,
                    reason,
                    data
                })
            ),
        (bytes.clone(), any::<u32>(), any::<u32>(), any::<u8>()).prop_map(
            |(filter, hash_functions, tweak, flags)| LegacyMessage::FilterLoad(FilterLoad {
                filter,
                hash_functions,
                tweak,
                flags
            })
        ),
        bytes.prop_map(LegacyMessage::FilterAdd),
        Just(LegacyMessage::FilterClear),
        Just(LegacyMessage::SendAddrV2),
        (any::<u16>(), any::<u16>(), any::<u64>()).prop_map(|(max, min, features)| {
            LegacyMessage::CompactVer(CompactVer {
                max_version: max,
                min_version: min,
                features,
            })
        }),
        proptest::collection::vec(any::<[u8; 32]>(), 0..3).prop_map(|ids| {
            LegacyMessage::Compact(hayai_relay::Message::BatchRequest(
                hayai_relay::BatchRequest {
                    ids: ids.into_iter().map(hayai_relay::BatchId).collect(),
                },
            ))
        }),
    ]
}

proptest! {
    #[test]
    fn round_trip(message in message()) {
        let frame = encode(Network::Mainnet, &message);
        prop_assert_eq!(decode(Network::Mainnet, &frame), Ok(message.clone()));
        let mut cursor = std::io::Cursor::new(&frame);
        let read = read_message(&mut cursor, Network::Mainnet, usize::MAX).unwrap();
        prop_assert_eq!(read, message);
        prop_assert_eq!(cursor.position() as usize, frame.len());
    }

    #[test]
    fn garbage_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..300), cmd in 0usize..23) {
        let commands: [&[u8; 12]; 23] = [
            hayai_net::codec::CMD_VERSION, hayai_net::codec::CMD_VERACK, hayai_net::codec::CMD_PING,
            hayai_net::codec::CMD_PONG, hayai_net::codec::CMD_ADDR, hayai_net::codec::CMD_GETADDR,
            hayai_net::codec::CMD_INV, hayai_net::codec::CMD_GETDATA, hayai_net::codec::CMD_NOTFOUND,
            hayai_net::codec::CMD_TX, hayai_net::codec::CMD_BLOCK, hayai_net::codec::CMD_HEADERS,
            hayai_net::codec::CMD_GETHEADERS, hayai_net::codec::CMD_MEMPOOL, hayai_net::codec::CMD_REJECT,
            hayai_net::codec::CMD_FILTERLOAD, hayai_net::codec::CMD_FILTERADD, hayai_net::codec::CMD_FILTERCLEAR,
            hayai_net::codec::CMD_SENDADDRV2, hayai_net::codec::CMD_ZCMPCTVER, hayai_net::codec::CMD_ZCMPCT,
            hayai_net::codec::CMD_ADDRV2, b"nonsense\0\0\0\0",
        ];
        let _ = decode_body(Network::Mainnet, commands[cmd], &bytes);
        let _ = decode(Network::Mainnet, &bytes);
        // Same bytes behind a valid frame header exercise the body parsers.
        let mut frame = Vec::new();
        frame.extend_from_slice(&Network::Mainnet.magic());
        frame.extend_from_slice(commands[cmd]);
        frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        frame.extend_from_slice(&checksum(&bytes));
        frame.extend_from_slice(&bytes);
        let _ = decode(Network::Mainnet, &frame);
        let mut cursor = std::io::Cursor::new(&frame);
        let _ = read_message(&mut cursor, Network::Mainnet, usize::MAX);
    }
}
