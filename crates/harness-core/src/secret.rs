//! Protection for secrets at rest (provider API keys, the local API token).
//!
//! * Windows: encrypted with DPAPI (`CryptProtectData`, current-user scope),
//!   so the values in `settings.json` can only be decrypted by your Windows
//!   account on this machine. Stored as `dpapi:<base64>`.
//! * macOS: the app runs in the App Sandbox; `settings.json` lives in the
//!   app's private container with mode 0600. Stored as plain text.
//! * Linux (headless server): mode 0600 in your data dir. Plain text.

const PREFIX: &str = "dpapi:";

/// Value to write to disk.
pub fn protect(plain: &str) -> String {
    if plain.is_empty() {
        return String::new();
    }
    imp::protect(plain).unwrap_or_else(|| plain.to_string())
}

/// Value read from disk -> usable secret. Unprotected (legacy) values pass
/// through; values that cannot be decrypted (e.g. a settings file copied from
/// another Windows account) become empty, so the user is asked to re-enter them.
pub fn unprotect(stored: &str) -> String {
    match stored.strip_prefix(PREFIX) {
        Some(b64) => imp::unprotect(b64).unwrap_or_else(|| {
            tracing::warn!("a stored secret could not be decrypted for this user; it was cleared");
            String::new()
        }),
        None => stored.to_string(),
    }
}

pub fn is_protected(stored: &str) -> bool {
    stored.starts_with(PREFIX)
}

#[cfg(windows)]
mod imp {
    use base64::Engine as _;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    fn call(input: &[u8], encrypt: bool) -> Option<Vec<u8>> {
        let blob_in = CRYPT_INTEGER_BLOB { cbData: input.len() as u32, pbData: input.as_ptr() as *mut u8 };
        let mut blob_out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: input/output blobs are valid for the duration of the call;
        // the output buffer is allocated by the OS and released with LocalFree.
        let ok = unsafe {
            if encrypt {
                CryptProtectData(
                    &blob_in,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut blob_out,
                )
            } else {
                CryptUnprotectData(
                    &blob_in,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut blob_out,
                )
            }
        };
        if ok == 0 || blob_out.pbData.is_null() {
            return None;
        }
        // SAFETY: the OS returned cbData valid bytes at pbData.
        let out = unsafe { std::slice::from_raw_parts(blob_out.pbData, blob_out.cbData as usize).to_vec() };
        unsafe {
            LocalFree(blob_out.pbData as _);
        }
        Some(out)
    }

    pub fn protect(plain: &str) -> Option<String> {
        call(plain.as_bytes(), true).map(|b| format!("{}{}", super::PREFIX, B64.encode(b)))
    }

    pub fn unprotect(b64: &str) -> Option<String> {
        let bytes = B64.decode(b64.trim()).ok()?;
        String::from_utf8(call(&bytes, false)?).ok()
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn protect(_plain: &str) -> Option<String> {
        None
    }

    pub fn unprotect(_b64: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for s in ["", "gsk_abc123", "ünïcødé-key"] {
            assert_eq!(unprotect(&protect(s)), s);
        }
        assert_eq!(unprotect("legacy-plain"), "legacy-plain");
    }

    #[cfg(windows)]
    #[test]
    fn encrypted_on_windows() {
        let p = protect("sk-very-secret");
        assert!(is_protected(&p), "{p}");
        assert!(!p.contains("sk-very-secret"));
        assert_eq!(unprotect("dpapi:not-valid-base64!!"), "");
    }

    #[cfg(not(windows))]
    #[test]
    fn plain_elsewhere() {
        assert_eq!(protect("x"), "x");
        assert!(!is_protected("x"));
    }
}
