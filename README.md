# Rant of a newly made NEET

As an almost lifelong Pythonist and the required enterprise Java (-.-) I don't speak a lick of Rust but I've tried dabbing in it over the recent years. Those stints never stuck since I valued the productivity I got out of Python more I suppose... Yeah yeah, learning curve skill issues on my part.

DHH's spiel refocused Rust back on the radar but more as a vibe-coding target that I can whip some stuff up for.

Anyways here's the rationale behind this quick project:
I gotta bunch of Macbook 2012s (Retina and non-Retina) idk I had a Mac phase where I decided to collect some older machines in 2021. Despite my reseating / reapplying thermal paste on the CPU (and GPU for Retina model) these guys be screaming at me when I'm doing dev + browsing on them.
I've tried to religiously monitor my Activity Monitor to make sure nothing's up. I'm on SSD equivalents and the RAM's okayish but all in all, all these Electron based apps and variant of Chromium (as well as being on unsupported OSes via OpenCore), they're just not having it.

Here's what I'm testing this on:

<img width="254" height="188" alt="Screenshot 2026-10-04 at 9 17 10 AM" src="https://github.com/user-attachments/assets/b7f935c0-8480-4b63-bd85-02adb28caa5f" />


So here we are, exploring a rust based browser for interacting with my _fav_ app. _Gemini_

I needed the 5TB space guys. So now I'm along for the ride in this Google AI world.

I'm also this close to dropping a 20 on Claude and supporting the Anthropic overlords.

Don't even ask about Cursor...

Anyways so far this has been a successful endeavor for my little Macbooks. They are happy, they can view remotely generated LLM generated text without having to blow a fuse on that Electron/Chromium bus.
Heck even WebKit fired up the fans so I can't just drag Chromium / V8 alone for this...

Now I can use _Gemini_ not in a browser and it's not going to make my old revitalized laptop take off ✈️ (this emoji isn't LLM generated!)

# Setup & Installation

If you are downloading a pre-built release for your operating system, it is a single native binary. No Python or pip is required. Sign-in uses a browser you already have installed (Chrome, Edge, Brave, Chromium, Firefox, or Safari). Nothing downloads a second Chromium.

## 1. Run the App & Get Cookies
1. Open the downloaded `Gemini Native Client` app (or `.exe` on Windows).
2. Type any simple message (e.g., "Hello") and press Enter.
3. The app will detect you don't have cookies yet. Chrome, Edge, Brave, Chromium, and Firefox open a separate window with its own profile. Safari uses the session already stored in Safari, and opens Safari only if those cookies are not there yet. Safari is left running.
4. **Log in to your Google Account** in that window if it is not signed in already. The separate profile is only for this app. The window stays open until sign-in finishes, so a password prompt or a second factor can take its time.
5. Once both session cookies (`__Secure-1PSID` and `__Secure-1PSIDTS`) are present, the app stores them and the window continues. A separate sign-in window closes itself. Safari does not.

You can skip the window by setting `GEMINI_PSID` and `GEMINI_PSIDTS` yourself (the same two cookie values, from the browser's cookie store).

## Key Features & Stability
- **Resilient Dual-Cookie Authentication**: The app keeps both `__Secure-1PSID` and `__Secure-1PSIDTS` before it treats sign-in as finished. While a request is running, the pane names the step it is actually on (checking the saved session, opening the browser, reading the reply, and so on).
- **In-Flight Request Cancellation**: Every active request displays a **⏹ Cancel** button right next to the busy indicator, letting you abort a stuck or slow prompt and close a sign-in browser.
- **Timeouts**: A generation request waits up to 120 seconds. Sign-in itself can take up to 10 minutes while the browser window is open.
- **One-Click Session Reset**: Hit **🗑 Clear Session** in the top menu to drop the in-memory clients, delete the stored session cookies, and remove the sign-in profile. Downloaded images and video in `~/.gemini_local/media` are left in place. The disk cleanup runs off the UI thread.
- **Automatic Task Teardown**: Closing or dropping a conversation pane cancels its in-flight request. A detached watcher stops a sign-in browser if that browser has detached from the app.

## Sending a message

The desktop window shows a reply when Gemini's stream finishes. The first reply after login pays for session init, then generation. Later messages in that pane reuse the same client. Cancel drops that client. A later send signs in again only if the saved cookies are gone.

If a pane stays busy, the status line names the step it is stuck on. Hit **⏹ Cancel** and send again.

## For Developers (Building from source)

```bash
cargo test
cargo run
```

The repo pins Rust 1.99 in `rust-toolchain.toml`. Rust 1.100 rejects `objc2` 0.4, which the window crate still uses, so `cargo` from this directory selects 1.99. The first build needs CMake on `PATH`. The Chrome TLS library compiles BoringSSL from source, and that step is the long one. Later builds reuse it. Run `cargo` from the repo root.

### Layout

| Module | Responsibility |
| --- | --- |
| `app` | Window state, history, folders |
| `pane` | One conversation, its input, and its in-flight task |
| `gemini` | Signed-in HTTP client: init, generate, download |
| `protocol` | Page token, request body, length-prefixed stream |
| `browser` | Sign-in. Split into `cookies`, `login`, `devtools`, and `readers` |
| `safety` | Domain, cookie, path, and redirect checks |
| `runtime` | One IO worker, plus a two-thread pool for short blocking calls |
| `reap` | Stop a sign-in browser without a shell |
| `paths` | Media files have to stay inside the media directory |
| `status` | The one-line activity label |
| `monitor` | Optional CPU / RAM sample thread |

`browser` is the piece that used to be one file. Cookie checks, browser launch, the DevTools socket, and the Firefox/Safari parsers now live apart so each can be tested without launching Gemini.

### Concurrency

The UI thread stays inside the winit loop. Network tasks are spawned onto a Tokio runtime with **one** worker. That worker polls every pane's request, so several conversations proceed together without a thread per core (those stacks were sitting idle on the machines this is for).

Anything that blocks — writing the cookie file, reading Firefox's sqlite database, spawning a process — goes to a blocking pool of two threads and returns. The sign-in loop itself sleeps with `tokio::time::sleep`. A long sign-in does not pin a thread between cookie reads, so another pane can keep streaming.

### Tests

`cargo test` runs both layers. It does not call Gemini.

- **Unit tests** live next to the code. They cover the page token, the request body, the length-prefixed stream, cookie parsing, the sign-in wait (including two polls running at once), path checks, and activity labels.
- **Integration tests** in `tests/` drive the window state, the cookie file through the public API, the runtime (overlapping tasks, and blocking work that must not stall the worker), and a scan of the repository for committed secrets.

`cargo test -- --ignored` is the live Gemini call. It uses whatever session is already on the machine and is skipped in normal runs.

### Security

Session cookies are not part of this repository.

- The saved session is `~/.gemini_local/cookies.json`. On Unix the file is mode `0600` and the directory is mode `0700`. Writes go to a temporary file and then rename, so a crash does not leave a half-written world-readable file.
- Values are checked before they are stored, loaded, or placed on a request. A value that could add extra cookie attributes is rejected. Debug output prints `[redacted]` instead of the value.
- `GEMINI_PSID` and `GEMINI_PSIDTS` are read from the environment only. They are not logged.
- `.gitignore` excludes `.env`, `cookies.json`, key files, and credential files. `cargo test` walks the tree and fails if a private key or a token-shaped string is committed.
- DevTools and WebDriver calls stay on loopback, and media downloads only follow `https` links on the image and video hosts Gemini uses.
- The release workflow's `GITHUB_TOKEN` is the short-lived token GitHub Actions mints for that job. It is not a user session and it is not stored in the repo.

## macOS app

`./build_mac_app.sh` builds a double-clickable app. It does not install `cargo-bundle`. That crate's current release compiles hundreds of unrelated packages (code signing, AV1, image codecs) only to wrap a `.app`.

The script:

1. Builds `gemini_native_client` and the small `gemini_reap` helper, then places both in `Contents/MacOS`. The helper closes a sign-in window if the app is killed outright. Cancel still works without it. For this script the release profile uses thin LTO and 8 codegen units, so the compile can finish on older CPUs. `Cargo.toml` still has full LTO and one codegen unit for a plain `cargo build --release`.
2. Assembles `target/release/bundle/osx/Gemini Native Client.app` with `assets/icon.icns`.

Install that bundle with:

```bash
cp -R "target/release/bundle/osx/Gemini Native Client.app" /Applications/
```

Rerun `./build_mac_app.sh` after changing the Rust sources, then copy the new bundle over the one in `/Applications`. Quit the app completely before opening the new copy. A running process keeps the binary it launched with.

## Cache locations in your home directory

`CACHE_DIR` = `~/.gemini_local`  
`COOKIE_FILE` = `~/.gemini_local/cookies.json` (mode `0600` on Unix)  
`PROFILE_DIR` = `~/.gemini_local/browser_profile`  
`WEBAPI_CACHE_DIR` = `~/.gemini_local/gemini_webapi_cache`  
`UI State File`: `~/Library/Application Support/Gemini-Native-Client/app.ron` (macOS) or `%APPDATA%/Gemini-Native-Client/app.ron` (Windows)


# Final Results

<img width="1920" height="1142" alt="Screenshot 2026-10-04 at 10 28 06 AM" src="https://github.com/user-attachments/assets/64a79a64-7c62-4f17-9b38-d6740fb0e86d" />

<img width="824" height="19" alt="Screenshot 2026-10-04 at 10 29 39 AM" src="https://github.com/user-attachments/assets/1716a469-3906-4f7f-acce-4cb5b0e5c6f9" />
<img width="767" height="21" alt="Screenshot 2026-10-04 at 10 29 01 AM" src="https://github.com/user-attachments/assets/7fb1906d-1173-48f2-aca2-a99908ec3414" />

Sign-in no longer downloads Playwright's Chromium. The window speaks the Gemini page protocol itself, with a Chrome 145 TLS fingerprint, so there is no Python helper to boot between messages.

