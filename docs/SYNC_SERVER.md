# Run your own sync server

Personal Harness syncs chats, providers, API keys and settings between your phone and computers through a small server that **you** run. The server is *zero-knowledge*:

- Everything is encrypted on your devices before upload, with XChaCha20-Poly1305 and a key derived from your password with Argon2id.
- Your password never leaves your devices. The server stores only a hash of a separate login key.
- Record names are keyed hashes, so the server cannot even tell a chat from a setting.
- Someone who copies the server's database gets unreadable data (verified in CI, see `crates/harness-core/tests/sync_e2e.rs`).

It is a single small binary with a SQLite database. Pick one of the options below.

## Option A — Fly.io (HTTPS included, free allowance)

```bash
# from the repository root, after installing flyctl and running `fly auth login`
fly launch --copy-config --config deploy/sync-server/fly.toml --dockerfile deploy/sync-server/Dockerfile --no-deploy
fly volumes create harness_sync_data --size 1
fly deploy --config deploy/sync-server/fly.toml --dockerfile deploy/sync-server/Dockerfile
```

Your server URL is `https://<app-name>.fly.dev`. Enter it in **Account & Sync** in the app.

## Option B — any VPS with Docker

```bash
docker build -f deploy/sync-server/Dockerfile -t harness-sync .
docker run -d --name harness-sync --restart unless-stopped -p 127.0.0.1:8787:8787 -v harness-sync:/data harness-sync
```

Put HTTPS in front of it. For example, with [Caddy](https://caddyserver.com), the whole `Caddyfile` is:

```
sync.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

## Option C — a computer at home (LAN or Tailscale)

```bash
cargo run --release -p harness-sync-server -- --addr 0.0.0.0:8787 --data ~/harness-sync-data
```

Use `http://<computer-ip>:8787` as the server URL. The apps allow plain `http://` **only** for private addresses: `localhost`, `192.168.x.x`, `10.x.x.x`, `172.16–31.x.x`, Tailscale `100.64–127.x.x` and `*.local`. Anything on the public internet must use `https://`. With [Tailscale](https://tailscale.com) your phone can sync from anywhere without exposing the server publicly.

## Who can create accounts

| Setting | Behaviour |
|---|---|
| *(default)* | Only the **first** account can be created: yours. All later sign-ups are refused. |
| `SIGNUP_CODE=some-long-code` | Anyone who knows the code can create an account. Share it with family and friends. |

Other environment variables:
- `QUOTA_MB`: storage per account, default 512.
- `TRUST_PROXY=1`: use `Fly-Client-IP` / `X-Forwarded-For` for rate limiting. Set this only behind a proxy you control.
- `ADDR`: listen address, default `0.0.0.0:8787`.
- `DATA_DIR`: data folder, default `./sync-data`.

## Security notes

- **Brute force:** sign-in is locked for 15 minutes after 10 failures (per IP and per email).
- **Account probing:** the server answers unknown emails exactly like known ones, so it doesn't reveal who has an account.
- **Sessions** last 180 days and are stored hashed. *Sign out* revokes them; *Delete account* requires the password and erases every record.
- **Backups:** copy `sync.db` from the data folder. It contains only encrypted records.
- **Forgotten password:** nobody can recover the synced copy, not even the server owner. Data on each device is untouched, so sign in with a new account and it is re-uploaded.
