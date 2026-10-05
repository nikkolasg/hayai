//! Cookie authentication of the JSON-RPC server, with the file and the header rule of
//! Zakura (`zakura-rpc/src/server/cookie.rs`, `server/http_request_compatibility.rs`).
//!
//! The node writes `__cookie__:<secret>` to the file `.cookie` at its start. A client
//! sends the content of the file as HTTP Basic credentials.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hayai_crypto::rng::{os_rng, RngCore};
use hayai_crypto::subtle::ConstantTimeEq;

/// Name of the cookie file in the cookie directory.
pub const COOKIE_FILE: &str = ".cookie";
/// The user name in the cookie file.
const COOKIE_USER: &str = "__cookie__";

/// The secret of one run of the server, and its file. The drop removes the file.
pub struct Cookie {
    secret: Arc<str>,
    path: PathBuf,
}

impl Cookie {
    /// Makes a secret of 32 bytes from the random source of the operating system and
    /// writes the cookie file in `dir`. The file has the mode 0600 on Unix from its
    /// creation: the node creates a new temporary file and renames it. The rename replaces
    /// a file that an earlier run left.
    pub fn create(dir: &Path) -> io::Result<Self> {
        let mut rng = os_rng();
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        let secret = base64_encode(&bytes);
        fs::create_dir_all(dir)?;
        let path = dir.join(COOKIE_FILE);
        let temporary = dir.join(format!("{COOKIE_FILE}.{:016x}.tmp", rng.next_u64()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let written = options
            .open(&temporary)
            .and_then(|mut file| file.write_all(format!("{COOKIE_USER}:{secret}").as_bytes()))
            .and_then(|()| fs::rename(&temporary, &path));
        if let Err(e) = written {
            let _ = fs::remove_file(&temporary);
            return Err(e);
        }
        Ok(Self {
            secret: secret.into(),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn secret(&self) -> Arc<str> {
        self.secret.clone()
    }
}

impl Drop for Cookie {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            tracing::warn!(path = %self.path.display(), error = %e, "cannot remove the RPC cookie file");
        }
    }
}

/// The header rule of Zakura (`check_credentials`): the second word of the value of
/// `Authorization` is the base64 form of `user:password`, and the password must be the
/// secret. The rule does not read the scheme and the user name. The comparison of the
/// password with the secret takes constant time.
pub(crate) fn accepts(secret: &str, authorization: Option<&str>) -> bool {
    let credentials = authorization
        .and_then(|value| value.split_whitespace().nth(1))
        .and_then(base64_decode)
        .and_then(|bytes| String::from_utf8(bytes).ok());
    match credentials.as_deref().and_then(|c| c.split(':').nth(1)) {
        Some(password) => password.as_bytes().ct_eq(secret.as_bytes()).into(),
        None => false,
    }
}

/// Client side: the value of the `Authorization` header for the cookie file `file`.
pub fn authorization(file: &Path) -> io::Result<String> {
    let credentials = fs::read(file)?;
    Ok(format!("Basic {}", base64_encode(&credentials)))
}

// The workspace has no base64 crate in its dependency tree. These two functions are the
// standard alphabet of RFC 4648 with padding.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut group = [0u8; 4];
        group[1..=chunk.len()].copy_from_slice(chunk);
        let bits = u32::from_be_bytes(group);
        for i in 0..4 {
            out.push(match i <= chunk.len() {
                true => ALPHABET[(bits >> (18 - 6 * i)) as usize & 63] as char,
                false => '=',
            });
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let padding = text.bytes().rev().take_while(|b| *b == b'=').count();
    if !text.len().is_multiple_of(4) || padding > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for chunk in text.as_bytes()[..text.len() - padding].chunks(4) {
        let mut bits = 0u32;
        for byte in chunk {
            bits = bits << 6 | ALPHABET.iter().position(|a| a == byte)? as u32;
        }
        bits <<= 6 * (4 - chunk.len());
        out.extend_from_slice(&bits.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test vectors of RFC 4648, section 10.
    #[test]
    fn base64_has_the_vectors_of_rfc_4648() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), encoded);
            assert_eq!(base64_decode(encoded), Some(plain.as_bytes().to_vec()));
        }
        assert_eq!(base64_encode(&[0xfb, 0xff]), "+/8=");
        for bad in ["Zg", "Zg=", "Z===", "Zm9v====", "Zm9-", "=Zg=", "Zm 9v"] {
            assert_eq!(base64_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_header_rule_takes_the_password_of_the_second_word() {
        let secret = "c2VjcmV0";
        let header = |credentials: &str| format!("Basic {}", base64_encode(credentials.as_bytes()));
        assert!(accepts(secret, Some(&header("__cookie__:c2VjcmV0"))));
        // As Zakura, the rule does not read the user name.
        assert!(accepts(secret, Some(&header("other:c2VjcmV0"))));
        for credentials in [
            "__cookie__:c2VjcmV1",
            "__cookie__:c2VjcmV0x",
            "__cookie__:c2VjcmV",
            "__cookie__:",
            "__cookie__",
            "c2VjcmV0",
            "",
        ] {
            assert!(
                !accepts(secret, Some(&header(credentials))),
                "{credentials}"
            );
        }
        assert!(!accepts(secret, None));
        assert!(!accepts(secret, Some("")));
        assert!(!accepts(secret, Some("Basic")));
        assert!(!accepts(secret, Some("Basic !!!!")));
        // The credentials without the scheme word are not the second word.
        let bare = base64_encode(b"__cookie__:c2VjcmV0");
        assert!(!accepts(secret, Some(&bare)));
    }
}
