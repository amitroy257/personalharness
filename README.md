# Personal Harness

A personal, **OpenRouter-style** app for **macOS and Windows**. It brings together the **free tiers** of many LLM providers behind:

- **a chat app**, and
- **one local OpenAI-compatible API** (`http://127.0.0.1:4141/v1`) that Cursor, VS Code extensions, scripts and other tools can use.

It also **ranks the models you can actually use** (strongest, most used, hot and personally best) and recommends which one to use. Everything is stored **only on your computer**: inside the App Sandbox on macOS, and in your local Windows profile with API keys encrypted by DPAPI on Windows.

<p align="center"><img src="src-tauri/app-icon.png" width="96" alt="app icon"></p>

## What it does

| | |
|---|---|
| **Free-tier aggregation** | Built in: Groq, Google Gemini, Mistral, Cerebras, GitHub Models, NVIDIA NIM, Cloudflare Workers AI, SambaNova, Hugging Face, Zhipu GLM Flash. You can add any OpenAI-compatible endpoint, such as a local Ollama or LM Studio. |
| **OpenRouter-like routing** | Ask for `auto`, `auto:coding`, `auto:reasoning` or `auto:fast`, an exact `provider/model`, or a bare model name like `llama-3.3-70b`. If a route is rate-limited or down, the request moves to the **same model on another provider**, then to the **next-best model**. Rate-limited routes cool down (it honours `Retry-After`). OpenRouter's `models: [...]` fallback list also works. |
| **Rankings & recommendations** | Views: **Recommended** (General / Coding / Reasoning / Fast), **Strongest**, **Most used**, **Hot**, **My usage**. Each score explains *why*. |
| **Private by design** | macOS: App Sandbox. Windows: local (non-roaming) profile folder, secrets encrypted with DPAPI. Keys and chats never leave the machine; no account, no sync, no telemetry. The local API listens on loopback only and needs a token. |

## Install on macOS (DMG)

Every push builds a universal DMG (Apple Silicon and Intel) on GitHub Actions:

1. Open **Actions → "macOS app (DMG)"**, pick the latest green run, and download the **PersonalHarness-macOS-universal** artifact (a zip containing the `.dmg`). Tagged versions (`v*`) also attach the DMG to a **GitHub Release**.
2. Open the DMG and drag **Personal Harness** to **Applications**.
3. The app is ad-hoc signed, not notarized: there is no paid Apple Developer ID. The first time you open it, macOS will block it. Do one of the following:
   - Right-click the app, choose **Open**, then **Open** again; or
   - go to **System Settings → Privacy & Security** and click **Open Anyway**; or
   - in Terminal, run: `xattr -dr com.apple.quarantine "/Applications/Personal Harness.app"`

Closing the window leaves the app running in the **menu bar**, so the local API stays available. Quit it from the menu-bar icon or with ⌘Q.

## Install on Windows

Every push also builds Windows x64 installers (Windows 10 and 11; also runs on Windows on ARM through emulation):

1. Open **Actions → "Windows app (installer)"**, pick the latest green run, and download **PersonalHarness-Windows-x64**. Tagged versions attach the installers to the **GitHub Release**.
2. Run **`Personal Harness_0.1.0_x64-setup.exe`**. It installs for your user only, with no administrator prompt. An `.msi` is also provided if you prefer it, for example for managed installs.
3. The installer is not code-signed (that needs a paid certificate), so **SmartScreen** may say "Windows protected your PC". Click **More info → Run anyway**.

Closing the window keeps the app running in the **system tray** (near the clock), so the local API stays available. Left-click the tray icon to reopen the window; right-click it for **Quit**. Opening the app again from the Start menu brings back the running instance rather than starting a second copy. The installer adds the Microsoft Edge WebView2 runtime if it is missing (it is preinstalled on Windows 10/11).

## Get free keys

Open **Providers** in the app. Each card links to the provider's key page and lists its free-tier limits. Paste a key and click **Connect**: the app lists the chat models your key can use and filters out embeddings, speech, image and guard models.

| Provider | Key | Notes |
|---|---|---|
| Groq | https://console.groq.com/keys | Very fast, generous free plan |
| Google Gemini | https://aistudio.google.com/app/apikey | Use a project **without billing** to stay free |
| Cerebras | https://cloud.cerebras.ai/ | Fastest output; smaller context on the free tier |
| Mistral | https://console.mistral.ai/api-keys | Free "Experiment" plan |
| GitHub Models | https://github.com/settings/personal-access-tokens/new | Fine-grained token with **Models: read** |
| NVIDIA NIM | https://build.nvidia.com/settings/api-keys | Many open models |
| Cloudflare Workers AI | https://dash.cloudflare.com/profile/api-tokens | Also needs your Account ID |
| SambaNova | https://cloud.sambanova.ai/apis | |
| Hugging Face | https://huggingface.co/settings/tokens | Small monthly credits |
| Zhipu (GLM Flash) | https://open.bigmodel.cn/usercenter/apikeys | Only the free Flash models are listed |

> Free tiers change often, and many allow the provider to log or train on your prompts. Don't send secrets to a free tier.

## Use the local API

**Local API** in the app shows the base URL and your token.

```bash
curl http://127.0.0.1:4141/v1/chat/completions \
  -H "Authorization: Bearer $HARNESS_KEY" -H "Content-Type: application/json" \
  -d '{"model": "auto", "messages": [{"role": "user", "content": "Hello!"}]}'
```

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:4141/v1", api_key="ph-…")
r = client.chat.completions.create(model="auto:coding", messages=[{"role": "user", "content": "Write fizzbuzz in Rust"}])
print(r.model)  # the model that actually answered, e.g. "cerebras/zai-glm-4.7"
```

- `GET /v1/models` lists the virtual `auto*` models plus every `provider/model` you can use, each with its rank.
- `POST /v1/chat/completions` supports streaming (SSE pass-through), tools and every other OpenAI parameter. The `x-harness-model`, `x-harness-provider` and `x-harness-attempts` response headers show what happened.
- `GET /v1/rankings?view=recommended&preset=coding` returns the same rankings as the app, as JSON.

In other apps, choose "OpenAI-compatible" (custom base URL), set the base URL to `http://127.0.0.1:4141/v1`, use your token as the API key, and `auto` as the model.

## How the rankings work

Each model you can call gets four 0–100 public signals. Models served by several providers are merged; for example, Llama 3.3 70B on Groq, Cerebras and NVIDIA counts as one model with three routes.

| Signal | Sources |
|---|---|
| **Strength** | [LMArena](https://lmarena.ai) text and code scores, relative to the #1 model. These come from a [public daily JSON mirror](https://github.com/oolong-tea-2026/arena-ai-leaderboards); a snapshot covering Mar–Oct 2026 ships with the app. Optional: [Artificial Analysis](https://artificialanalysis.ai) indexes. Models missing from the leaderboards' top lists get a size-based **estimate**, shown as `~`, capped near the bottom of those lists. |
| **Popularity** ("most used") | Optional OpenRouter Data API daily token rankings (needs a free OpenRouter key, used only for this), Hugging Face 30-day downloads, LMArena vote counts |
| **Hot** | Hugging Face trending score, release recency (OpenRouter / Hugging Face / when the model first appeared on the leaderboard), OpenRouter usage growth |
| **Speed** | Your measured tokens/sec, otherwise a per-provider prior (Cerebras > Groq > SambaNova > …) |

Presets combine them:

- **General** = 55% strength + 25% popularity + 20% hot
- **Coding** = 60% coding strength + 20% popularity + 20% hot, plus a bonus for coder models
- **Reasoning** = 75% strength + 15% popularity + 10% hot, plus a bonus for thinking models
- **Fast** = speed × quality

Then **your own results** are blended in: success rate, speed and 👍/👎 in chat. They count in proportion to how much history exists, up to the weight you set (default 35%). A route that is cooling down drops to the bottom until it recovers.

## Privacy & data

- **Where data lives:**
  - macOS: `~/Library/Containers/io.github.amitroy257.personalharness/Data/Library/Application Support/io.github.amitroy257.personalharness/`
  - Windows: `%LOCALAPPDATA%\io.github.amitroy257.personalharness\` (the local, non-roaming profile, so it is never synced to other machines)
  - In both: `settings.json` holds keys, preferences and the local API token; `harness.db` (SQLite) holds chats, per-request metadata (model, status, timing, token counts) and ratings. Local API requests are logged as **metadata only, never prompt text**.
- **macOS sandbox:** the app has only the `app-sandbox`, `network.client` and `network.server` entitlements. It cannot read your files. `settings.json` is mode `0600`.
- **Windows protection:** Windows has no App Sandbox for ordinary desktop apps. Instead, provider keys, ranking keys and the local API token are **encrypted with DPAPI** (current-user scope), so only your Windows account on this PC can decrypt them. Copying `settings.json` to another account or machine yields nothing usable.
- **Network:** traffic goes only to the providers you connect, plus anonymous GETs for ranking data (`raw.githubusercontent.com`, `huggingface.co`, `openrouter.ai`). **Privacy & Settings** lists every host, and you can turn public data off to rank from the bundled snapshot plus your own usage.
- **Local API:** binds `127.0.0.1` only, requires the token, rejects non-loopback `Host` headers (which blocks DNS-rebinding), and never sends CORS headers, so web pages can't use it.
- **Delete:** **Privacy & Settings** can delete chats, usage history or everything.

## Build from source

Requirements: [Rust](https://rustup.rs), Node 20+, plus:
- macOS 11+: Xcode Command Line Tools
- Windows 10/11: [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with "Desktop development with C++"

```bash
npm ci
npm run tauri dev                              # run the app in development
npm run tauri build -- --bundles dmg           # macOS: .app + .dmg
npm run tauri build -- --bundles nsis,msi      # Windows: setup .exe + .msi
```

Without the desktop app (any OS), the same router and UI also run headless:

```bash
npm ci && npm run build
cargo run -p harness-cli --release -- --port 4141 --ui dist
# prints the API base URL, token, and a web UI link
```

Tests: `cargo test -p harness-core` runs unit tests plus an end-to-end test against mock providers: fallback, cooldowns, streaming, auth and chat persistence. CI runs them on Linux, macOS and Windows, then installs and launches the built apps as a smoke test.

Refresh the bundled leaderboard snapshot with `python3 scripts/build_baseline.py`.

## Project layout

```
crates/harness-core   Router, providers, ranking, local API, SQLite store (all logic, tested)
crates/harness-cli    `harness-server`: headless API + web UI
src-tauri             Desktop shell (macOS + Windows): window, menu bar / tray, sandbox entitlements, DMG / NSIS / MSI config
src                   React UI (Chat, Rankings, Models, Providers, Local API, Privacy & Settings)
scripts               Baseline leaderboard generator, icon generator
```
