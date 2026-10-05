//! Converts the checkpoint lists into the form that `src/checkpoints.rs` embeds.
//!
//! A source file (`src/checkpoints/*.txt`) has one line for each checkpoint: the height, a
//! space, and the block hash in its display form. The output file has 36 bytes for each
//! checkpoint: the height as a little-endian `u32`, then the 32 bytes of the hash in wire
//! order. The heights of a list increase.

use std::path::Path;
use std::{env, fs};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let out = env::var_os("OUT_DIR").expect("cargo sets OUT_DIR");
    for name in ["main-checkpoints", "test-checkpoints"] {
        let source = format!("src/checkpoints/{name}.txt");
        println!("cargo::rerun-if-changed={source}");
        let text = fs::read_to_string(&source).unwrap_or_else(|e| panic!("{source}: {e}"));
        let mut bytes = Vec::with_capacity(36 * text.lines().count());
        let mut previous: Option<u32> = None;
        for (n, line) in text.lines().enumerate() {
            let at = format!("{source}:{}", n + 1);
            let (height, hash) = line
                .split_once(' ')
                .unwrap_or_else(|| panic!("{at}: the line is not `height hash`"));
            let height: u32 = height
                .parse()
                .unwrap_or_else(|e| panic!("{at}: height: {e}"));
            assert!(previous < Some(height), "{at}: the heights must increase");
            previous = Some(height);
            assert!(
                hash.len() == 64 && hash.is_ascii(),
                "{at}: a block hash has 64 hex digits"
            );
            bytes.extend_from_slice(&height.to_le_bytes());
            // The display form has the bytes in the opposite order.
            for i in (0..32).rev() {
                let byte = u8::from_str_radix(&hash[2 * i..2 * i + 2], 16)
                    .unwrap_or_else(|e| panic!("{at}: hash: {e}"));
                bytes.push(byte);
            }
        }
        let target = Path::new(&out).join(format!("{name}.bin"));
        fs::write(&target, bytes).unwrap_or_else(|e| panic!("{}: {e}", target.display()));
    }
}
