//! Generated transparent scripts: pairs of a locking script and an unlocking script.

use ripemd::Ripemd160;
use sha2::{Digest, Sha256};

use crate::rng::Rng;

pub const OP_0: u8 = 0x00;
pub const OP_1: u8 = 0x51;
pub const OP_IF: u8 = 0x63;
pub const OP_ENDIF: u8 = 0x68;
pub const OP_DROP: u8 = 0x75;
pub const OP_DUP: u8 = 0x76;
pub const OP_EQUAL: u8 = 0x87;
pub const OP_NOT: u8 = 0x91;
pub const OP_HASH160: u8 = 0xa9;
pub const OP_CHECKSIG: u8 = 0xac;
pub const OP_CHECKMULTISIG: u8 = 0xae;
pub const OP_NOP1: u8 = 0xb0;
pub const OP_CLTV: u8 = 0xb1;

/// The compressed encoding of the generator of secp256k1: a valid public key.
const PUBLIC_KEY: [u8; 33] = [
    0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87, 0x0b,
    0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8, 0x17,
    0x98,
];

/// A locking script and the unlocking script of a spend.
#[derive(Clone, Debug)]
pub struct Pair {
    pub lock: Vec<u8>,
    pub unlock: Vec<u8>,
    /// A lock time that the pair is made for, when it has one.
    pub lock_time: Option<u32>,
}

/// The push of `data` in its shortest form.
pub fn push(data: &[u8]) -> Vec<u8> {
    let mut script = Vec::with_capacity(data.len() + 5);
    match data.len() {
        0..=75 => script.push(data.len() as u8),
        76..=255 => script.extend_from_slice(&[0x4c, data.len() as u8]),
        256..=65_535 => {
            script.push(0x4d);
            script.extend_from_slice(&(data.len() as u16).to_le_bytes());
        }
        _ => {
            script.push(0x4e);
            script.extend_from_slice(&(data.len() as u32).to_le_bytes());
        }
    }
    script.extend_from_slice(data);
    script
}

/// The bytes of the script number `n`: little-endian magnitude with a sign bit.
pub fn number(n: i64) -> Vec<u8> {
    if n == 0 {
        return Vec::new();
    }
    let mut magnitude = n.unsigned_abs();
    let mut bytes = Vec::new();
    while magnitude > 0 {
        bytes.push(magnitude as u8);
        magnitude >>= 8;
    }
    if bytes[bytes.len() - 1] & 0x80 != 0 {
        bytes.push(if n < 0 { 0x80 } else { 0 });
    } else if n < 0 {
        let last = bytes.len() - 1;
        bytes[last] |= 0x80;
    }
    bytes
}

pub fn hash160(data: &[u8]) -> [u8; 20] {
    Ripemd160::digest(Sha256::digest(data)).into()
}

/// The pay-to-script-hash locking script of `redeem`.
pub fn p2sh(redeem: &[u8]) -> Vec<u8> {
    let mut lock = vec![OP_HASH160];
    lock.extend(push(&hash160(redeem)));
    lock.push(OP_EQUAL);
    lock
}

/// One element of a generated script: an opcode, a push or a random byte.
fn element(rng: &mut Rng) -> Vec<u8> {
    match rng.below(10) {
        // The opcodes from OP_1NEGATE to OP_NOP10.
        0..=5 => vec![0x4f + rng.below(0xb9 - 0x4f + 1) as u8],
        6 => push(&number(
            [
                0i64,
                1,
                -1,
                2,
                16,
                17,
                127,
                128,
                255,
                256,
                32_767,
                0x7fff_ffff,
                0x8000_0000,
            ][rng.below(13)],
        )),
        7 => {
            let len = *rng.pick(&[0usize, 1, 2, 4, 5, 20, 32, 33, 75, 76]);
            push(&rng.bytes(len))
        }
        8 => vec![OP_0],
        _ => vec![rng.next_u64() as u8],
    }
}

fn soup(rng: &mut Rng, max: usize) -> Vec<u8> {
    let count = rng.below(max) + 1;
    (0..count).flat_map(|_| element(rng)).collect()
}

/// A signature in a form near DER, with a hash type byte: valid forms and broken forms.
fn signature(rng: &mut Rng) -> Vec<u8> {
    let integer = |rng: &mut Rng| -> Vec<u8> {
        let len = *rng.pick(&[0usize, 1, 31, 32, 33, 34]);
        let mut value = rng.bytes(len);
        match rng.below(4) {
            // A leading zero byte, as a positive number with the top bit set has.
            0 if !value.is_empty() => value[0] = 0,
            // A negative number.
            1 if !value.is_empty() => value[0] |= 0x80,
            _ => {}
        }
        value
    };
    let (r, s) = (integer(rng), integer(rng));
    let mut body = vec![0x02, r.len() as u8];
    body.extend(r);
    body.extend([0x02, s.len() as u8]);
    body.extend(s);
    let mut der = vec![0x30];
    match rng.below(6) {
        // A length in the long form.
        0 => der.extend([0x81, body.len() as u8]),
        // A length that is 1 too large or 1 too small.
        1 => der.push(body.len() as u8 + 1),
        2 => der.push((body.len() as u8).wrapping_sub(1)),
        _ => der.push(body.len() as u8),
    }
    der.extend(body);
    if rng.chance(1, 6) {
        der.extend(rng.some_bytes(1, 3));
    }
    if rng.chance(5, 6) {
        der.push(*rng.pick(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x80, 0x81, 0x82, 0x83, 0xff]));
    }
    der
}

fn public_key(rng: &mut Rng) -> Vec<u8> {
    match rng.below(6) {
        0 => rng.bytes(33),
        1 => {
            // An uncompressed or hybrid prefix with random coordinates.
            let mut key = rng.bytes(65);
            key[0] = *rng.pick(&[0x04, 0x06, 0x07]);
            key
        }
        2 => Vec::new(),
        3 => {
            let mut key = PUBLIC_KEY.to_vec();
            key[0] = *rng.pick(&[0x03, 0x04, 0x05, 0x00]);
            key
        }
        _ => PUBLIC_KEY.to_vec(),
    }
}

/// A pair of scripts. Most pairs are not valid. The pairs reach the limits and the
/// encodings on which two script interpreters can differ.
pub fn pair(rng: &mut Rng, height: u32, time: u32) -> Pair {
    let plain = |lock: Vec<u8>, unlock: Vec<u8>| Pair {
        lock,
        unlock,
        lock_time: None,
    };
    match rng.below(12) {
        // Random scripts.
        0 | 1 => {
            let unlock = if rng.chance(3, 4) {
                (0..rng.below(4)).flat_map(|_| element(rng)).collect()
            } else {
                soup(rng, 4)
            };
            plain(soup(rng, 12), unlock)
        }
        // Random scripts that end with a true value.
        2 => {
            let mut lock = soup(rng, 8);
            lock.push(OP_1);
            plain(lock, (0..rng.below(3)).flat_map(|_| element(rng)).collect())
        }
        // Pay to script hash.
        3 | 4 => {
            let redeem = match rng.below(4) {
                0 => vec![OP_1],
                1 => soup(rng, 8),
                2 => {
                    // Signature operations in a branch that does not run.
                    let mut redeem = vec![OP_0, OP_IF];
                    for _ in 0..rng.below(40) {
                        match rng.below(3) {
                            0 => redeem.push(OP_CHECKSIG),
                            1 => redeem.push(OP_CHECKMULTISIG),
                            _ => {
                                redeem.extend([0x50 + (1 + rng.below(16)) as u8, OP_CHECKMULTISIG])
                            }
                        }
                    }
                    redeem.extend([OP_ENDIF, OP_1]);
                    redeem
                }
                _ => {
                    let mut redeem = soup(rng, 6);
                    redeem.push(OP_1);
                    redeem
                }
            };
            let mut unlock: Vec<u8> = (0..rng.below(3)).flat_map(|_| element(rng)).collect();
            match rng.below(8) {
                // An unlocking script that is not push-only.
                0 => unlock.push(OP_NOP1),
                // Another script than the hash commits to.
                1 => unlock.extend(push(&[OP_1, OP_1])),
                _ => {}
            }
            let mut lock = p2sh(&redeem);
            unlock.extend(push(&redeem));
            if rng.chance(1, 10) {
                flip_one(rng, &mut lock);
            }
            plain(lock, unlock)
        }
        // OP_CHECKLOCKTIMEVERIFY.
        5 | 6 => {
            let operand = *rng.pick(&[
                0i64,
                -1,
                1,
                i64::from(height) - 1,
                i64::from(height),
                i64::from(height) + 1,
                499_999_999,
                500_000_000,
                i64::from(time) - 1,
                i64::from(time),
                i64::from(time) + 1,
                0x7fff_ffff,
                0x8000_0000,
                0xffff_ffff,
                0x1_0000_0000,
                0xff_ffff_ffff,
            ]);
            let mut lock = push(&number(operand));
            lock.extend([OP_CLTV, OP_DROP, OP_1]);
            let lock_time = match rng.below(4) {
                0 => rng.near_u32(&[height, 500_000_000, time]),
                _ => u32::try_from(operand.clamp(0, i64::from(u32::MAX)))
                    .expect("the operand is in the range")
                    .wrapping_add([0u32, 0, 1, u32::MAX][rng.below(4)]),
            };
            Pair {
                lock,
                unlock: Vec::new(),
                lock_time: Some(lock_time),
            }
        }
        // Signature checks with keys and signatures in valid and broken encodings. The
        // signatures do not verify, so the scripts negate the result.
        7 | 8 => {
            let mut lock = Vec::new();
            let mut unlock = Vec::new();
            if rng.chance(2, 3) {
                unlock.extend(push(&signature(rng)));
                lock.extend(push(&public_key(rng)));
                lock.push(OP_CHECKSIG);
            } else {
                let keys = 1 + rng.below(3);
                let sigs = rng.below(keys + 2);
                // The extra element that OP_CHECKMULTISIG removes.
                unlock.extend(if rng.chance(3, 4) {
                    vec![OP_0]
                } else {
                    element(rng)
                });
                for _ in 0..sigs {
                    let sig = if rng.chance(1, 3) {
                        Vec::new()
                    } else {
                        signature(rng)
                    };
                    unlock.extend(push(&sig));
                }
                lock.push(0x50 + sigs.min(16) as u8);
                for _ in 0..keys {
                    lock.extend(push(&public_key(rng)));
                }
                lock.extend([0x50 + keys as u8, OP_CHECKMULTISIG]);
            }
            if rng.chance(4, 5) {
                lock.push(OP_NOT);
            }
            plain(lock, unlock)
        }
        // The size limits of the interpreter.
        9 | 10 => match rng.below(5) {
            0 => {
                // The count of opcodes that are not pushes: the limit is 201.
                let mut lock = vec![OP_1];
                lock.extend(vec![OP_NOP1; 199 + rng.below(5)]);
                plain(lock, Vec::new())
            }
            1 => {
                // The size of a script: the limit is 10,000 bytes.
                let size = 9_998 + rng.below(5);
                let mut lock = Vec::with_capacity(size + 4);
                while lock.len() + 523 < size {
                    lock.extend(push(&[0u8; 510]));
                    lock.push(OP_DROP);
                }
                while lock.len() + 1 < size {
                    lock.push(0x61);
                    if lock.len() % 100 == 0 {
                        break;
                    }
                }
                let rest = size.saturating_sub(lock.len() + 1);
                if rest >= 4 {
                    lock.extend(push(&vec![0u8; rest - 4]));
                    lock.push(OP_DROP);
                }
                lock.push(OP_1);
                plain(lock, Vec::new())
            }
            2 => {
                // The size of a stack element: the limit is 520 bytes.
                let data = vec![1u8; 518 + rng.below(5)];
                plain(vec![OP_DROP, OP_1], push(&data))
            }
            3 => {
                // The size of the stack: the limit is 1,000 elements.
                let unlock = vec![OP_1; 997 + rng.below(6)];
                plain(vec![OP_1], unlock)
            }
            _ => {
                // The size of a number operand: the limit is 4 bytes.
                let operand = rng.some_bytes(3, 4);
                let mut lock = push(&operand);
                lock.extend([0x8b, OP_DROP, OP_1]);
                plain(lock, Vec::new())
            }
        },
        // Truth values: the encodings of zero.
        _ => {
            let value = match rng.below(6) {
                0 => vec![0x80],
                1 => vec![0x00, 0x80],
                2 => vec![0x00, 0x00],
                3 => vec![0x00, 0x01],
                4 => Vec::new(),
                _ => rng.some_bytes(1, 5),
            };
            let lock = match rng.below(3) {
                0 => vec![OP_IF, OP_1, 0x67, OP_0, OP_ENDIF],
                1 => vec![OP_DUP, OP_DROP],
                _ => vec![OP_NOT, OP_NOT],
            };
            plain(lock, push(&value))
        }
    }
}

fn flip_one(rng: &mut Rng, bytes: &mut [u8]) {
    if !bytes.is_empty() {
        let at = rng.below(bytes.len());
        bytes[at] ^= 1 << rng.below(8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_numbers_have_the_shortest_encoding() {
        assert_eq!(number(0), Vec::<u8>::new());
        assert_eq!(number(1), vec![1]);
        assert_eq!(number(-1), vec![0x81]);
        assert_eq!(number(127), vec![0x7f]);
        assert_eq!(number(128), vec![0x80, 0x00]);
        assert_eq!(number(-128), vec![0x80, 0x80]);
        assert_eq!(number(3_400_000), vec![0x40, 0xe1, 0x33]);
    }

    #[test]
    fn pushes_use_the_shortest_opcode() {
        assert_eq!(push(&[]), vec![0]);
        assert_eq!(push(&[7; 75])[0], 75);
        assert_eq!(&push(&[7; 76])[..2], &[0x4c, 76]);
        assert_eq!(&push(&[7; 256])[..3], &[0x4d, 0x00, 0x01]);
    }
}
