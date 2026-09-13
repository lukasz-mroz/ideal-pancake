//! At-rest encryption for secrets in the settings table (API keys).
//!
//! The database is a plain file in the portable folder, so a key stored there
//! is a key anyone who copies the folder can read. On Windows we wrap secrets
//! with DPAPI, which ties the ciphertext to the current user account: copied
//! to another machine or another user, it will not decrypt. Elsewhere the
//! value is stored as-is (the portable build is Windows-only; other platforms
//! keep the previous behaviour rather than gaining a false sense of secrecy).
//!
//! Values written by this module carry a `dpapi:` prefix. A value without it
//! is treated as legacy plaintext and returned unchanged, so existing
//! databases keep working and are re-encrypted the next time they are saved.

const PREFIX: &str = "dpapi:";

/// Settings whose values are encrypted at rest.
pub fn is_secret_key(key: &str) -> bool {
    matches!(
        key,
        "api_key_claude"
            | "api_key_open_ai"
            | "api_key_gemini"
            | "api_key_elevenlabs"
    )
}

/// Encrypt a secret for storage. Empty stays empty (an unset key is not a
/// secret), and anything already wrapped is left alone.
pub fn protect(value: &str) -> String {
    if value.is_empty() || value.starts_with(PREFIX) {
        return value.to_string();
    }
    match encrypt(value.as_bytes()) {
        Some(bytes) => format!("{}{}", PREFIX, base64_encode(&bytes)),
        None => value.to_string(),
    }
}

/// Decrypt a stored secret. A value without the prefix is legacy plaintext.
pub fn reveal(value: &str) -> String {
    let Some(encoded) = value.strip_prefix(PREFIX) else {
        return value.to_string();
    };
    match base64_decode(encoded).and_then(|bytes| decrypt(&bytes)) {
        Some(plain) => String::from_utf8_lossy(&plain).into_owned(),
        None => {
            log::warn!("Could not decrypt a stored secret - it may have been copied from another user or machine");
            String::new()
        }
    }
}

#[cfg(target_os = "windows")]
fn encrypt(data: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::HLOCAL;
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    use windows::Win32::System::Memory::LocalFree;

    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();

        CryptProtectData(&mut input, None, None, None, None, 0, &mut output).ok()?;

        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let owned = slice.to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Some(owned)
    }
}

#[cfg(target_os = "windows")]
fn decrypt(data: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::HLOCAL;
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    use windows::Win32::System::Memory::LocalFree;

    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();

        CryptUnprotectData(&mut input, None, None, None, None, 0, &mut output).ok()?;

        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let owned = slice.to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Some(owned)
    }
}

#[cfg(not(target_os = "windows"))]
fn encrypt(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(target_os = "windows"))]
fn decrypt(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

// A tiny standard-alphabet base64, to avoid pulling the value through another
// dependency just for storage encoding.
fn base64_encode(input: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(CHARS[((n >> 18) & 63) as usize] as char);
        out.push(CHARS[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let cleaned: Vec<u8> = input.bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::with_capacity(cleaned.len() / 4 * 3);
    for chunk in cleaned.chunks(4) {
        let mut n = 0u32;
        let mut bits = 0;
        for &c in chunk {
            n = (n << 6) | val(c)?;
            bits += 6;
        }
        n <<= 24 - bits;
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}
