# Personal Harness — Quality, Security & Privacy Test Report

| | |
|---|---|
| **Product** | Personal Harness 0.1.0 (macOS universal DMG, Windows x64 NSIS/MSI, Android APK, headless server, self-hosted sync server) |
| **Report date** | 2026-11-04 |
| **Prepared by** | SQA review (Claude, acting as senior SQA engineer) |
| **Verdict** | ✅ **PASS for personal use**, with the residual risks in §7 |
| **Live evidence** | Every CI run's **Summary** tab shows a pass/fail table: [CI](https://github.com/amitroy257/personalharness/actions/workflows/ci.yml) · [macOS](https://github.com/amitroy257/personalharness/actions/workflows/macos.yml) · [Windows](https://github.com/amitroy257/personalharness/actions/workflows/windows.yml) |

---

## 1. Executive summary

The headline requirement is **"data must stay sandboxed on the local device."** The installed apps verify this on real macOS and Windows machines in CI, by trying to break the guarantees rather than by reading the configuration:

- **macOS:** the installed app runs inside the **App Sandbox**. On every build, CI launches the shipped app and has it try to list your home folder, read `~/Documents` and `~/Desktop`, and write a file into your home folder. **Every attempt must be denied, or the build fails.** All app data lives in `~/Library/Containers/io.github.amitroy257.personalharness/`, and CI also fails if the app's data folder appears outside the container.
- **Windows:** ordinary desktop apps have no App-Sandbox equivalent, so the compensating controls are tested instead:
  - data lives in the **local (non-roaming)** profile;
  - every API key and the local token are **encrypted with DPAPI**. CI proves this by reading the raw file and decrypting it with Windows' own `ProtectedData` API.

The review found **5 defects in the shipped code, one rated High**: deleted chats could be recovered from SQLite's write-ahead log. **All 5 are fixed and guarded by regression tests.** Another 5 issues were found and fixed during development. Dependency audits are clean.

## 2. Scope and approach

**In scope:**
- request routing and fallback;
- rankings;
- the local OpenAI-compatible API;
- local storage;
- privacy and sandboxing;
- rendering of untrusted model output in the UI;
- installers and first launch on macOS and Windows;
- third-party dependencies.

**Approach:** risk-based. The highest-risk claims (privacy, sandbox, API exposure) are tested *adversarially*: each test attempts the attack and passes only if the attack is blocked. Everything runs automatically on three operating systems for every commit, so the results don't decay.

**Not covered** (see §7):
- live calls to real LLM providers, which were unreachable from the test environment, so routing is tested against mock providers that follow their documented formats;
- live Hugging Face and OpenRouter endpoints;
- Intel Mac hardware (the universal binary is verified with `lipo`, but CI runs on Apple Silicon);
- Windows on ARM;
- accessibility;
- long-duration soak tests.

## 3. Test environments

| Environment | Runs |
|---|---|
| **Linux** (GitHub `ubuntu-latest`) | Rust unit + end-to-end + privacy tests, UI security tests, rustfmt / clippy `-D warnings`, desktop crate compile, npm + cargo dependency audit |
| **macOS 14, Apple Silicon** (GitHub `macos-14`) | Rust tests; universal app + DMG build; signature / entitlement / architecture checks; **launch of the built app and in-app sandbox escape probes** |
| **Windows** (GitHub `windows-latest`) | Rust tests (including real DPAPI); NSIS + MSI build; **silent per-user install, launch, DPAPI decryption check, privacy self-check, single-instance check** |
| **Exploratory** (Linux, headless server + mock providers + Chromium/Playwright) | Full UI walkthrough; live attack with malicious model output (with capture of every network request the page made); performance probes with 100k usage rows; live LMArena data fetch |

## 4. Results

Legend: ✅ pass · ➖ not applicable on this platform (with the compensating control named) · 🔍 verified by inspection

### 4.1 Data sandboxing & privacy (critical)

| ID | Requirement | How it is verified | macOS | Windows | Linux |
|---|---|---|:-:|:-:|:-:|
| P-01 | App data stays in the app's private location | Installed-app CI: data in `~/Library/Containers/<id>` (macOS) / `%LOCALAPPDATA%\<id>` (Windows); macOS CI fails if anything is created outside the container | ✅ | ✅ | n/a |
| P-02 | **The OS sandbox blocks access to your files** | The running app tries to list `~`, `~/Documents`, `~/Desktop` and to write `~/.personal-harness-sandbox-probe-*`; all must be denied (`selfcheck::escape_probes`) | ✅ | ➖ no OS sandbox for desktop apps → P-03, P-04 | ➖ headless mode is not sandboxed |
| P-03 | Secrets encrypted at rest | Windows: CI reads the raw `settings.json`, asserts the token is `dpapi:…`, decrypts it with `ProtectedData.Unprotect(CurrentUser)` and uses it; unit test `secret::encrypted_on_windows` | ➖ plaintext inside the sandbox container, 0600 | ✅ | ➖ 0600 |
| P-04 | Owner-only files (data dir 0700, files 0600, including SQLite `-wal`/`-shm`) | In-app self-check + `store::database_files_are_owner_only` | ✅ | ➖ profile ACL | ✅ |
| P-05 | **Deleted chats cannot be recovered from disk** | `store::deleted_chats_are_not_recoverable_from_disk` scans the raw database, WAL and SHM bytes after deleting one chat and after deleting all chats | ✅ | ✅ | ✅ |
| P-06 | Each provider key is sent only to its own provider | e2e mock asserts the exact `Authorization` header per provider | ✅ | ✅ | ✅ |
| P-07 | The local API token and the client's own headers/cookies are never forwarded upstream | e2e mock records every upstream header and asserts none of them leak | ✅ | ✅ | ✅ |
| P-08 | "Public data off" ⇒ **zero** outbound ranking requests | `privacy::public_data_off_makes_zero_requests` (recording server) | ✅ | ✅ | ✅ |
| P-09 | Optional ranking keys reach only their own service; anonymous sources get no credentials and only a generic `PersonalHarness/x.y` user agent | `privacy::keys_only_reach_their_own_service` | ✅ | ✅ | ✅ |
| P-10 | API requests are logged as metadata only (never prompt text) | 🔍 the `usage` table has no content column; only chats you type in the app are stored | 🔍 | 🔍 | 🔍 |
| P-11 | No telemetry | 🔍 the only outbound calls are to providers you configure plus the ranking sources listed in Privacy & Settings; P-08 and P-09 cover the ranking client | 🔍 | 🔍 | 🔍 |
| P-12 | API keys are never sent to the UI layer | e2e: the providers view contains only masked hints (`…groq`) | ✅ | ✅ | ✅ |

#### Evidence: privacy self-check output from the installed macOS app

Recorded in CI on GitHub's `macos-14` runner ([run 41928682482](https://github.com/amitroy257/personalharness/actions/runs/41928682482), commit `d887c89`): **11 passed, 0 failed**. "Operation not permitted" is the App Sandbox denying the request at the kernel level.

| Check | Result | Evidence |
|---|---|---|
| macOS App Sandbox active | ✅ pass | Data folder: /Users/runner/Library/Containers/io.github.amitroy257.personalharness/Data/Library/Application Support/io.github.amitroy257.personalharness |
| Cannot read your home folder | ✅ pass | /Users/runner: Operation not permitted (os error 1) |
| Cannot read ~/Documents | ✅ pass | Operation not permitted (os error 1) |
| Cannot read ~/Desktop | ✅ pass | Operation not permitted (os error 1) |
| Cannot write outside its container | ✅ pass | /Users/runner/.personal-harness-sandbox-probe-5014: Operation not permitted (os error 1) |
| Keys & token at rest | ℹ️ info | Stored in the sandbox container (other apps cannot read it), file mode 0600 |
| Data folder is owner-only (0700) | ✅ pass | 700 |
| Settings & database files are owner-only (0600) | ✅ pass | 4 files checked |
| Local API unreachable from the network | ✅ pass | connect to 192.168.64.5:4141 refused |
| Local API rejects requests without the token | ✅ pass | unauthenticated request -> HTTP 401 |
| Local API rejects foreign Host headers (DNS rebinding) | ✅ pass | Host: attacker.example -> HTTP 403 |
| Web pages cannot call the local API (no CORS) | ✅ pass | no Access-Control-Allow-Origin header |

#### Evidence: privacy self-check output from the installed Windows app

Recorded in CI on GitHub's `windows-latest` runner after a silent per-user install ([run 41928682403](https://github.com/amitroy257/personalharness/actions/runs/41928682403), commit `d887c89`): **6 passed, 0 failed**. In the same run, CI read the raw `settings.json`, decrypted the token with Windows `ProtectedData.Unprotect(CurrentUser)`, and used it to call the API successfully. A second launch left exactly **1** running instance.

| Check | Result | Evidence |
|---|---|---|
| Data in local (non-roaming) profile | ✅ pass | C:\Users\runneradmin\AppData\Local\io.github.amitroy257.personalharness |
| OS sandbox | ℹ️ info | Windows desktop apps have no App-Sandbox equivalent; protection is the per-user profile plus DPAPI encryption of secrets |
| Keys & token encrypted at rest (DPAPI) | ✅ pass | 1 secret(s) on disk, 0 unencrypted |
| Local API unreachable from the network | ✅ pass | connect to 10.1.0.106:4141 refused |
| Local API rejects requests without the token | ✅ pass | unauthenticated request -> HTTP 401 |
| Local API rejects foreign Host headers (DNS rebinding) | ✅ pass | Host: attacker.example -> HTTP 403 |
| Web pages cannot call the local API (no CORS) | ✅ pass | no Access-Control-Allow-Origin header |
| No telemetry | ℹ️ info | No analytics or crash reporting. Public ranking data is on (anonymous GETs). |

### 4.2 Local API exposure

All checks run inside the installed app on macOS and Windows (`/v1/selfcheck`), and in the e2e test on every OS.

| ID | Requirement | Result |
|---|---|:-:|
| S-01 | Unreachable from the network: connecting to the machine's LAN IP on the API port is refused | ✅ |
| S-02 | Requests without the token get `401` | ✅ |
| S-03 | DNS-rebinding defence: a foreign `Host` header gets `403` | ✅ |
| S-04 | No CORS headers, so web pages cannot read responses | ✅ |
| S-05 | Constant-time token comparison; loopback host parsing rejects `127.0.0.1.evil.com` | ✅ unit |

### 4.3 Untrusted model output in the UI (XSS)

A model can return arbitrary text, and inside the desktop app a successful script injection could call app commands.

| ID | Attack | Unit (`src/lib/format.test.ts`) | Live UI attack (Playwright) |
|---|---|:-:|:-:|
| U-01 | `<script>` | ✅ | ✅ not executed |
| U-02 | `<img onerror>`, `<svg onload>` | ✅ | ✅ no handlers in the DOM |
| U-03 | `javascript:` links | ✅ | ✅ removed |
| U-04 | Remote image / tracking pixel | ✅ | ✅ **0 external requests** |
| U-05 | `<iframe>`, `<form>`, inline `style` | ✅ | ✅ |
| U-06 | Clicking a link must not navigate the app away | — | ✅ opens in the system browser instead |

Defence in depth: the desktop CSP allows scripts only from the app itself, images only from the app or `data:`, and network access only to the app's IPC channel (🔍 `src-tauri/tauri.conf.json`).

### 4.4 Routing (OpenRouter-style behaviour)

| ID | Behaviour | Result |
|---|---|:-:|
| R-01 | A rate-limited (429) model falls back to the **same model on another provider** | ✅ e2e |
| R-02 | Rate-limited route cools down (`Retry-After` honoured), so the next request skips it with no wasted call | ✅ e2e |
| R-03 | OpenRouter-style `models: [...]` fallback list, `auto` / `auto:<preset>`, bare model names | ✅ e2e + unit |
| R-04 | Streaming (SSE) pass-through ends with `[DONE]`; usage recorded | ✅ e2e |
| R-05 | Unknown model → `404` with a helpful message | ✅ e2e |
| R-06 | OpenRouter-only request fields are stripped before forwarding | ✅ e2e + unit |
| R-07 | The same model under different provider IDs is recognised as one model (Groq / Cerebras / NVIDIA / Hugging Face / Cloudflare naming) | ✅ 5 unit tests |
| R-08 | Model discovery filters out embeddings, speech, image and guard models | ✅ unit |
| R-09 | Chat persistence, ratings, "My usage" ranking | ✅ e2e |

### 4.5 Ranking correctness

| ID | Behaviour | Result |
|---|---|:-:|
| K-01 | One model served by several providers ranks as **one** entry with multiple routes | ✅ |
| K-02 | Strength order follows the leaderboard; models missing from it are flagged *estimated* and capped | ✅ |
| K-03 | Fast preset favours fast providers; personal success and 👍/👎 move the ranking; cooling-down models drop | ✅ |
| K-04 | Offline or failed sources fall back to the bundled snapshot and never empty the rankings | ✅ |
| K-05 | Live data: the LMArena mirror has published empty days since 2026-10-07; the client steps back to the last good snapshot | ✅ verified against the live mirror |

### 4.6 Installers & platform behaviour

| ID | Check | macOS | Windows |
|---|---|:-:|:-:|
| I-01 | Builds succeed | ✅ universal DMG (~15 MB) | ✅ NSIS `.exe` (5.4 MB) + `.msi` (7.6 MB) |
| I-02 | Binary architectures | ✅ `x86_64 arm64` | ✅ x64 |
| I-03 | Signature / entitlements | ✅ ad-hoc signed, `app-sandbox` + `network.client` + `network.server` only | ➖ unsigned (see R-2) |
| I-04 | Install without admin | ➖ drag to Applications | ✅ per-user silent install to `%LOCALAPPDATA%\Personal Harness` |
| I-05 | Launches and serves the API | ✅ in ~2 s | ✅ in ~2 s |
| I-06 | A second launch reuses the running instance | ✅ (handled by the OS) | ✅ exactly 1 process after a second launch |

### 4.7 Performance

| Scenario | Before | After |
|---|---|---|
| Ranking computation per `auto` request, with 100,000 usage rows | 166–178 ms (SQL aggregates on every request) | **0.67 ms** (aggregates cached for 15 s and invalidated on rating or delete; at most one 178 ms refresh per 15 s) |

### 4.8 Supply chain

| Check | Result |
|---|---|
| `npm audit --omit=dev` (code shipped in the app) | ✅ 0 vulnerabilities |
| `npm audit` (build and test tooling) | ✅ 0, after the F5 fix |
| `cargo audit` (RustSec: 540 crates against 1,403 advisories) | ✅ **0 vulnerabilities**. 5 warnings (4 *unmaintained*, 1 *unsound*: `glib`, `zvariant`, `zvariant_derive`, `zbus_names`, `proc-macro-error`), all in the **Linux-only** GTK / D-Bus dependency chain. Verified with `cargo tree` that none of these crates is in the dependency graph of the Apple Silicon, Intel Mac or Windows builds. Tracked; resolves when Tauri updates its Linux stack |

### 4.9 End-to-end encrypted sync (zero knowledge)

Design: password → Argon2id → HKDF splits it into a *login key* and a *wrapping key*. The server keeps only an Argon2 hash of the login key. A random data key, wrapped by the wrapping key, encrypts every record with XChaCha20-Poly1305. The record id is bound in as associated data, and record names are HMACs. Verified by `tests/sync_e2e.rs` (two real devices plus a real server) and `harness-sync-server/tests/server.rs`.

| ID | Requirement | Result |
|---|---|:-:|
| Y-01 | **The server stores only ciphertext.** After a full two-device session, the server's raw database/WAL bytes are scanned for chat text, titles, API keys, settings names and the password. **None are found.** | ✅ |
| Y-02 | A wrong password can neither sign in nor unwrap the key. A server cannot swap records between ids (AEAD over the record id, plus a re-derived id check) or alter them (tamper → decryption fails) | ✅ |
| Y-03 | Devices converge: initial upload of existing data, restore on a new device, ratings, new chats, settings, **provider keys** (and their model lists), deletions, concurrent edits (most recent edit wins, deterministic tie-break) | ✅ |
| Y-04 | A fresh device's default settings never overwrite the account's settings | ✅ |
| Y-05 | Device-specific values (local API port and token) are never synced | ✅ |
| Y-06 | Server: sign-up limited to the owner or an invite code; 10-failure lockout; identical answers for unknown emails (no account enumeration); every sync endpoint needs a session; sign-out revokes it; deleting the account needs the password and leaves no record bytes on disk; quota and input validation | ✅ |
| Y-07 | The client refuses plain `http://` to public hosts, passwords under 10 characters, and servers that ask for weakened key derivation (< 8 MiB) | ✅ |
| Y-08 | The local session token and data key on disk are owner-only (Unix) or DPAPI-encrypted (Windows) | ✅ |
| Y-09 | Live UI run: laptop creates the account; phone (Pixel 7 viewport) signs in and sees every chat; chats with the synced keys; the laptop then sees the phone's chat; no console errors | ✅ exploratory |

### 4.10 Android

<!-- ANDROID_EVIDENCE -->

### 4.11 Code-quality gates (block merges)

`rustfmt --check` · `clippy -D warnings` · TypeScript `strict` · 10 UI tests · Rust tests on every push: **42/42 Linux, 42/42 macOS, 40/40 Windows**. The counts differ because 3 Unix-only permission tests don't apply on Windows, and 1 Windows-only DPAPI test runs there instead.

## 5. Defects found by this review

| ID | Severity | Finding | Root cause | Fix | Regression test |
|---|---|---|---|---|---|
| **F1** | **High** (privacy) | After **Delete chat** / **Delete all chats**, the deleted text was still readable in `harness.db-wal` | SQLite keeps old page images in the write-ahead log until a checkpoint; `VACUUM` alone does not clear them | `PRAGMA secure_delete=ON`, plus `wal_checkpoint(TRUNCATE)` after every delete and wipe | `store::deleted_chats_are_not_recoverable_from_disk` |
| **F2** | Medium | `harness.db-wal` / `-shm` were created world-readable (`0644`) | SQLite copies the database file's mode, and that mode was tightened only *after* opening | Create the database file `0600` before opening; repair the mode of existing side files at startup | `store::database_files_are_owner_only` |
| **F3** | Medium (performance) | 166–178 ms of overhead per `auto` request with a large history | Usage aggregates recomputed on every request | 15 s cache with explicit invalidation | §4.7 measurement |
| **F4** | Low | `<svg>` elements survived sanitising (their handlers were already stripped) | Allow-list too broad | Forbid `svg`/`math` in rendered output | `format.test.ts` |
| **F5** | Medium (supply chain, dev only) | Test runner vitest 3 pulled in 2 critical + 1 moderate advisories | Vulnerable transitive dependencies (`tinypool`, `@vitest/mocker`) | Upgrade to vitest 5 | `npm audit` CI step |

Found while testing sync and the phone version:

| ID | Severity | Finding | Fix | Regression test |
|---|---|---|---|---|
| **F6** | Medium | Messages saved in the same millisecond appeared in a different order on other devices, because the tie-break was local insertion order | Strictly increasing timestamps within each conversation | `sync_e2e` |
| **F7** | Medium | A new device received the synced provider keys but never fetched those providers' model lists, so it had keys but no models | Refresh model lists of providers that arrive through sync | `sync_e2e` + live UI run |
| **F8** | Medium (UX) | On phones, wide code blocks pushed chat bubbles off-screen, and the 7th tab was unreachable. The first automated check missed it because it only measured page overflow; it now measures every bubble and the message pane | Shrinkable grid items, code scrolls inside its block, short tab labels | Playwright overflow checks |
| **F9** | High (privacy, preventive) | Android backup could copy app data (including API keys) to cloud storage or another phone | Manifest hardened (`allowBackup=false`); the emulator test asserts the flag is off | `android_smoke.sh` |

Found and fixed earlier, during development testing:
- "Hot" ranking was empty when Hugging Face or OpenRouter were unreachable. Fixed with a fallback to the date each model first appeared on the leaderboard.
- Estimated strength was anchored to the single weakest leaderboard entry. It now uses the 10th percentile.
- Custom providers without a key (e.g. Ollama) were unusable.
- On Windows, launching twice caused a port conflict. Fixed with single-instance mode.
- Windows data was placed in the roaming profile, which can sync to company servers. It now uses `%LOCALAPPDATA%`.

## 6. System design review

```
 ┌──────────── desktop app (Tauri) ─────────────┐        ┌──────── your tools ────────┐
 │  UI (WebView, strict CSP)                    │        │ Cursor / scripts / SDKs    │
 │     │ IPC: call(name,args) / chat(channel)   │        └────────────┬───────────────┘
 │  harness-core ───────────────────────────────┼── 127.0.0.1:4141 ──┘ token + Host check
 │   router ─ fallback, cooldowns, SSE relay    │
 │   ranking ─ public signals + local stats     │──► free-tier providers (your keys)
 │   store ─ SQLite (secure delete, 0600)       │──► ranking sources (anonymous GET, optional)
 │   settings ─ DPAPI (Win) / sandbox (mac)     │
 └──────────────────────────────────────────────┘
```

**Strengths:**
- **One core, three front ends.** The desktop app, the headless server and the local API share `harness-core`, so one tested code path serves every entry point.
- **Secrets stay out of the UI.** The webview receives only masked key hints.
- **Fails safe.** Rankings fall back to a bundled snapshot. A per-route cooldown acts as a circuit breaker so rate-limited providers aren't hammered. Streams have idle and header timeouts.
- **Least privilege on macOS.** Three entitlements, no file access, loopback-only server.
- **Self-verifying.** The privacy self-check ships in the product (Privacy & Settings → *Run privacy check*), so users can re-verify the guarantees on their own machine.

## 7. Residual risks & recommendations

| # | Risk | Rating | Recommendation |
|---|---|---|---|
| R-1 | Behaviour against **real** providers is not exercised by CI (mocks follow documented formats) | **High** | Before relying on it, run the manual checklist in §8 with your own keys. Optionally add a nightly CI job with test keys stored as repository secrets |
| R-2 | Builds are **not notarized (macOS) or Authenticode-signed (Windows)**: Gatekeeper/SmartScreen warnings, and no publisher identity proving the download wasn't tampered with | Medium | Developer ID + notarization; Windows code-signing certificate; publish SHA-256 checksums with releases |
| R-3 | Free-tier providers may log or train on prompts | Medium (policy) | The app warns about this. Don't send secrets to free tiers; prefer providers with clear data policies, or a local Ollama provider for sensitive work |
| R-4 | **Windows:** chats in `harness.db` are plaintext in your profile. Other user accounts can't read them, but malware running *as you* could (DPAPI protects keys, not chat history, and doesn't stop same-user malware) | Medium | Future: encrypt the database (e.g. SQLCipher with a DPAPI-protected key) |
| R-5 | Requests rejected with HTTP 400 are retried on up to `max_attempts` routes, which can spend quota on a malformed request | Low | Accepted. Configurable in Local API → Routing |
| R-6 | LMArena data comes from a community mirror that has been stale since 2026-10-07 | Low | Bundled snapshot plus walk-back. Artificial Analysis (optional key) gives an independent strength source |
| R-7 | No auto-update, so security fixes need a manual reinstall | Low | Add the Tauri updater once releases are signed |
| R-8 | Sync metadata visible to the server: your email, number and size of records, timing, IP address | Low (inherent) | Self-host; use Tailscale or a private network for extra privacy |
| R-9 | "Most recent edit wins" depends on device clocks; a badly wrong clock can win a conflict it shouldn't | Low | Keep automatic time enabled on devices |
| R-10 | Android: keys are protected by the app sandbox but not by the Android Keystore; a rooted phone could read them | Low | Future: wrap secrets with an Android Keystore key |
| R-11 | Android builds use a temporary signing key until you add the signing secrets, so updates then need an uninstall (sync restores the data) | Low | Add the three signing secrets (README → Android) |

## 8. Manual acceptance checklist (with real keys)

1. Install, open **Providers**, connect Groq and Gemini. Each card should show "Connected — found N chat models".
2. **Chat** with `auto`. The reply footer should show which `provider/model` answered.
3. **Rankings → Recommended / Strongest / Hot**. Lists should be populated; "Why?" should explain each score.
4. **Local API**:
   ```bash
   curl http://127.0.0.1:4141/v1/chat/completions \
     -H "Authorization: Bearer <token>" -H "Content-Type: application/json" \
     -d '{"model":"auto","messages":[{"role":"user","content":"hi"}]}'
   ```
   It should answer, and the `x-harness-model` header should be set.
5. **Privacy & Settings → Run privacy check**. Expect no failures. On macOS, expect the four escape attempts to show ✓.
6. Delete a chat. It should be gone after restarting the app.

## 9. How to reproduce

```bash
cargo test -p harness-core -p harness-cli                # 42 tests: unit, e2e, privacy
npx vitest run                                           # UI sanitiser tests
cargo clippy --workspace --all-targets -- -D warnings
npm audit --omit=dev
```

The installed-app checks (sandbox escape probes, DPAPI, single instance) run in the macOS and Windows workflows. Each run's **Summary** tab lists every check with its evidence.
