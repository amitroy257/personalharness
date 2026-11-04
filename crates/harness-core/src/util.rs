use std::io::Write;
use std::path::Path;

/// Every HTTP client in the app is built here. On Android, certificates are
/// verified against the bundled Mozilla root store: the OS verifier would need
/// JNI initialisation from Java, and failing open is not an option.
pub fn http_builder() -> reqwest::ClientBuilder {
    let b = reqwest::Client::builder();
    #[cfg(target_os = "android")]
    let b = b.tls_certs_only(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().filter_map(|c| reqwest::Certificate::from_der(c.as_ref()).ok()),
    );
    b
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Random local API token, e.g. `ph-3f9a…` (32 hex chars of entropy).
pub fn new_token() -> String {
    let bytes: [u8; 16] = rand::random();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("ph-{hex}")
}

/// Atomically write a file readable only by the current user.
pub fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `"sk-abcdef123456"` -> `"…3456"`; empty stays empty.
pub fn key_hint(key: &str) -> String {
    let k = key.trim();
    if k.is_empty() {
        return String::new();
    }
    let tail: String = k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{tail}")
}

pub fn truncate_chars(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

/// Slug for custom provider ids.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helpers() {
        assert_eq!(key_hint("gsk_abcdef1234"), "…1234");
        assert_eq!(key_hint(""), "");
        assert_eq!(slugify("My Local Ollama!"), "my-local-ollama");
        assert_eq!(truncate_chars("hello world", 5), "hello…");
        assert_eq!(new_token().len(), 35);
    }
}
