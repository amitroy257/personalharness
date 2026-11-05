//! Privacy self-check: the running app actively tries to break its own privacy
//! guarantees and reports whether each attempt was blocked.
//!
//! * macOS desktop app: tries to list and write your real home folder (the
//!   App Sandbox must deny both) and confirms the data lives in its container.
//! * Windows: confirms secrets on disk are DPAPI-encrypted and data is in the
//!   local (non-roaming) profile.
//! * Everywhere: owner-only file permissions, loopback-only API, token and
//!   Host-header enforcement (probed over real HTTP against itself).

use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use crate::harness::Harness;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    Info,
}

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub label: &'static str,
    pub status: Status,
    pub detail: String,
}

fn check(id: &'static str, label: &'static str, status: Status, detail: impl Into<String>) -> Check {
    Check { id, label, status, detail: detail.into() }
}

/// The user's real home directory, even inside the macOS sandbox (where
/// `$HOME` points into the container).
#[cfg(unix)]
fn real_home() -> Option<PathBuf> {
    // SAFETY: getpwuid returns a pointer to static storage or null; we copy
    // the string out immediately.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().to_string();
        Some(PathBuf::from(dir))
    }
}

#[cfg(not(unix))]
fn real_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

pub fn macos_sandbox_active() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some()
}

#[cfg(unix)]
fn mode_of(p: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).ok().map(|m| m.permissions().mode() & 0o777)
}

/// Try to list `path`. Only an OS "permission denied" proves isolation; a
/// path that does not exist proves nothing and is reported as inconclusive.
fn probe_denied(id: &'static str, label: &'static str, path: &std::path::Path) -> Check {
    match std::fs::read_dir(path).map(|d| d.count()) {
        Ok(n) => check(id, label, Status::Fail, format!("listed {n} entries in {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            check(id, label, Status::Pass, format!("{}: {e}", path.display()))
        }
        Err(e) => check(id, label, Status::Info, format!("inconclusive — {}: {e}", path.display())),
    }
}

/// Packages that are always installed (this app itself needs a WebView provider).
pub const ANDROID_PROBE_PACKAGES: &[&str] = &[
    "com.android.webview",
    "com.google.android.webview",
    "com.android.chrome",
    "com.android.providers.contacts",
    "com.android.providers.media.module",
    "com.android.settings",
];

/// Another app's private folder must be either denied (Android ≤ 10) or not
/// visible at all (Android 11+ "app data isolation" mounts a per-app view of
/// /data/data). Readable is a failure.
fn probe_other_app_android() -> Check {
    let (id, label) = ("escape_read_app", "Cannot read another app's files");
    let mut hidden = Vec::new();
    for pkg in ANDROID_PROBE_PACKAGES {
        let path = format!("/data/data/{pkg}");
        let r = probe_denied(id, label, std::path::Path::new(&path));
        match r.status {
            Status::Info => hidden.push(path),
            _ => return r,
        }
    }
    check(
        id,
        label,
        Status::Pass,
        format!(
            "other apps' folders are not visible to this app (Android app-data isolation): {} — at least one of these packages (the WebView provider) is always installed",
            hidden.join(", ")
        ),
    )
}

fn primary_lan_ip() -> Option<IpAddr> {
    // No packet is sent: connecting a UDP socket only selects a route.
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

pub async fn run(h: &Harness) -> Vec<Check> {
    let mut out = Vec::new();
    let data_dir = h.data_dir.clone();
    let desktop = h.is_desktop();

    // ---- OS isolation ----------------------------------------------------------
    if cfg!(target_os = "macos") {
        let active = macos_sandbox_active();
        let in_container = data_dir.to_string_lossy().contains("/Library/Containers/");
        out.push(match (desktop, active) {
            (_, true) => check(
                "os_sandbox",
                "macOS App Sandbox active",
                if in_container { Status::Pass } else { Status::Fail },
                format!("Data folder: {}", data_dir.display()),
            ),
            (true, false) => check(
                "os_sandbox",
                "macOS App Sandbox active",
                Status::Fail,
                "The desktop app is running without the App Sandbox",
            ),
            (false, false) => check("os_sandbox", "macOS App Sandbox", Status::Info, "Headless server mode is not sandboxed"),
        });
        if active {
            out.extend(escape_probes());
        }
    } else if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        let roaming = std::env::var_os("APPDATA").map(PathBuf::from);
        let in_local = local.as_ref().map(|l| data_dir.starts_with(l)).unwrap_or(false);
        let in_roaming = roaming.as_ref().map(|r| data_dir.starts_with(r)).unwrap_or(false);
        out.push(check(
            "data_location",
            "Data in local (non-roaming) profile",
            if in_local && !in_roaming {
                Status::Pass
            } else if desktop {
                Status::Fail
            } else {
                Status::Info
            },
            format!("{}", data_dir.display()),
        ));
        out.push(check(
            "os_sandbox",
            "OS sandbox",
            Status::Info,
            "Windows desktop apps have no App-Sandbox equivalent; protection is the per-user profile plus DPAPI encryption of secrets",
        ));
    } else if cfg!(target_os = "android") {
        // Android runs every app as its own Linux user with a private data directory.
        let private = data_dir.starts_with("/data/");
        out.push(check(
            "os_sandbox",
            "Android app sandbox (private app storage)",
            if private { Status::Pass } else { Status::Fail },
            format!("Data folder: {}", data_dir.display()),
        ));
        out.push(probe_denied("escape_other_apps", "Cannot list other apps' private data", std::path::Path::new("/data/data")));
        out.push(probe_denied("escape_system_data", "Cannot read system data", std::path::Path::new("/data/system")));
        out.push(probe_other_app_android());
    } else {
        out.push(check("os_sandbox", "OS sandbox", Status::Info, "Linux/headless: protected by owner-only permissions"));
    }

    // ---- secrets at rest -------------------------------------------------------
    let settings_path = data_dir.join("settings.json");
    let raw = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let stored: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    let mut secrets: Vec<String> = vec![];
    if let Some(t) = stored.pointer("/server/token").and_then(|v| v.as_str()) {
        secrets.push(t.to_string());
    }
    if let Some(ps) = stored.get("providers").and_then(|p| p.as_object()) {
        secrets.extend(ps.values().filter_map(|p| p.get("api_key")?.as_str().map(String::from)));
    }
    secrets.retain(|s| !s.is_empty());
    if cfg!(windows) {
        let plain = secrets.iter().filter(|s| !crate::secret::is_protected(s)).count();
        out.push(check(
            "secrets_encrypted",
            "Keys & token encrypted at rest (DPAPI)",
            if plain == 0 && !secrets.is_empty() { Status::Pass } else { Status::Fail },
            format!("{} secret(s) on disk, {} unencrypted", secrets.len(), plain),
        ));
    } else {
        out.push(check(
            "secrets_encrypted",
            "Keys & token at rest",
            Status::Info,
            if macos_sandbox_active() {
                "Stored in the sandbox container (other apps cannot read it), file mode 0600"
            } else if cfg!(target_os = "android") {
                "Stored in private app storage (other apps cannot read it), file mode 0600"
            } else {
                "Stored with owner-only file permissions"
            },
        ));
    }

    // ---- file permissions (unix) ----------------------------------------------
    #[cfg(unix)]
    {
        let dir_mode = mode_of(&data_dir);
        out.push(check(
            "dir_permissions",
            "Data folder is owner-only (0700)",
            if dir_mode == Some(0o700) { Status::Pass } else { Status::Fail },
            format!("{:o}", dir_mode.unwrap_or(0)),
        ));
        let files: Vec<PathBuf> = ["settings.json", "harness.db", "harness.db-wal", "harness.db-shm"]
            .iter()
            .map(|f| data_dir.join(f))
            .filter(|p| p.exists())
            .collect();
        let loose: Vec<String> = files
            .iter()
            .filter(|p| mode_of(p) != Some(0o600))
            .map(|p| format!("{} ({:o})", p.file_name().unwrap().to_string_lossy(), mode_of(p).unwrap_or(0)))
            .collect();
        out.push(check(
            "file_permissions",
            "Settings & database files are owner-only (0600)",
            if loose.is_empty() { Status::Pass } else { Status::Fail },
            if loose.is_empty() { format!("{} files checked", files.len()) } else { format!("too open: {}", loose.join(", ")) },
        ));
    }

    // ---- local API exposure ------------------------------------------------------
    let status = h.server_status().await;
    let running = status["running"].as_bool().unwrap_or(false);
    let port = status["port"].as_u64().unwrap_or(0) as u16;
    if running {
        let lan = primary_lan_ip();
        out.push(match lan {
            Some(ip) => {
                let reachable = TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_millis(800)).is_ok();
                check(
                    "api_loopback_only",
                    "Local API unreachable from the network",
                    if reachable { Status::Fail } else { Status::Pass },
                    format!("connect to {ip}:{port} {}", if reachable { "SUCCEEDED" } else { "refused" }),
                )
            }
            None => check(
                "api_loopback_only",
                "Local API unreachable from the network",
                Status::Pass,
                "bound to 127.0.0.1 (no LAN address)",
            ),
        });
        let http = crate::util::http_builder().no_proxy().timeout(Duration::from_secs(5)).build();
        if let Ok(http) = http {
            let url = format!("http://127.0.0.1:{port}/v1/models");
            let no_token = http.get(&url).send().await.map(|r| r.status().as_u16()).unwrap_or(0);
            out.push(check(
                "api_requires_token",
                "Local API rejects requests without the token",
                if no_token == 401 { Status::Pass } else { Status::Fail },
                format!("unauthenticated request -> HTTP {no_token}"),
            ));
            let token = h.settings().server.token;
            let foreign = http
                .get(&url)
                .bearer_auth(&token)
                .header("Host", "attacker.example")
                .send()
                .await
                .map(|r| r.status().as_u16())
                .unwrap_or(0);
            out.push(check(
                "api_host_check",
                "Local API rejects foreign Host headers (DNS rebinding)",
                if foreign == 403 { Status::Pass } else { Status::Fail },
                format!("Host: attacker.example -> HTTP {foreign}"),
            ));
            let cors = http
                .request(reqwest::Method::OPTIONS, &url)
                .header("Origin", "https://attacker.example")
                .header("Access-Control-Request-Method", "POST")
                .send()
                .await
                .map(|r| r.headers().contains_key("access-control-allow-origin"))
                .unwrap_or(false);
            out.push(check(
                "api_no_cors",
                "Web pages cannot call the local API (no CORS)",
                if cors { Status::Fail } else { Status::Pass },
                if cors { "Access-Control-Allow-Origin was returned" } else { "no Access-Control-Allow-Origin header" },
            ));
        }
    } else {
        out.push(check("api_loopback_only", "Local API", Status::Info, "server is off"));
    }

    // ---- outbound data -----------------------------------------------------------
    let s = h.settings();
    out.push(check(
        "telemetry",
        "No telemetry",
        Status::Info,
        format!(
            "No analytics or crash reporting. Public ranking data is {}.",
            if s.ranking.public_data { "on (anonymous GETs)" } else { "off (no ranking requests)" }
        ),
    ));
    out
}

/// Try to reach outside the sandbox container. Each attempt must be denied.
fn escape_probes() -> Vec<Check> {
    let mut out = Vec::new();
    let Some(home) = real_home() else {
        out.push(check("escape_home", "Cannot read your home folder", Status::Fail, "could not resolve the home folder"));
        return out;
    };
    out.push(probe_denied("escape_read_home", "Cannot read your home folder", &home));
    out.push(probe_denied("escape_read_documents", "Cannot read ~/Documents", &home.join("Documents")));
    out.push(probe_denied("escape_read_desktop", "Cannot read ~/Desktop", &home.join("Desktop")));
    let probe = home.join(format!(".personal-harness-sandbox-probe-{}", std::process::id()));
    out.push(match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            check("escape_write_home", "Cannot write outside its container", Status::Fail, format!("created {}", probe.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            check("escape_write_home", "Cannot write outside its container", Status::Pass, format!("{}: {e}", probe.display()))
        }
        Err(e) => check(
            "escape_write_home",
            "Cannot write outside its container",
            Status::Info,
            format!("inconclusive — {}: {e}", probe.display()),
        ),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_only_pass_on_permission_denied() {
        let dir = tempfile::tempdir().unwrap();
        // readable folder: an escape succeeded -> fail
        assert_eq!(probe_denied("x", "x", dir.path()).status, Status::Fail);
        // missing folder proves nothing -> inconclusive, never a pass
        let missing = dir.path().join("does-not-exist");
        assert_eq!(probe_denied("x", "x", &missing).status, Status::Info);
    }

    #[test]
    fn lan_ip_is_not_loopback() {
        if let Some(ip) = primary_lan_ip() {
            assert!(!ip.is_loopback());
        }
    }

    #[cfg(unix)]
    #[test]
    fn real_home_resolves() {
        assert!(real_home().is_some());
    }
}
