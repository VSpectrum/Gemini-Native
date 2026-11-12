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
4. **Log in to your Google Account** if that window is not signed in already.
5. Once both session cookies (`__Secure-1PSID` and `__Secure-1PSIDTS`) are present, the app stores them and the window continues. A separate sign-in window closes itself. Safari does not.

You can skip the window by setting `GEMINI_PSID` and `GEMINI_PSIDTS` yourself (the same two cookie values, from the browser's cookie store).

## Key Features & Stability
- **Resilient Dual-Cookie Authentication**: Specifically verifies both `__Secure-1PSID` and timestamped `__Secure-1PSIDTS` cookies before concluding sign-in, preventing silent auth drops or endless "Thinking (or checking cookies)" hangs.
- **In-Flight Request Cancellation**: Every active request displays a **⏹ Cancel** button right next to the busy indicator, letting you abort a stuck or slow prompt and close a sign-in browser.
- **Timeouts**: A generation request waits up to 120 seconds. Sign-in itself can take up to 5 minutes while the browser window is open.
- **One-Click Session Reset**: Hit **🗑 Clear Session** in the top menu to wipe stored session cookies and the sign-in profile if credentials ever need a refresh.
- **Automatic Task Teardown**: Closing or dropping a conversation pane cancels its in-flight request. A detached watcher stops a sign-in browser if that browser has detached from the app.

## Sending a message

The desktop window shows a reply when Gemini's stream finishes. The first reply after login pays for session init, then generation. Later messages in that pane reuse the same client. Cancel drops that client. A later send signs in again only if the saved cookies are gone.

If a pane stays on Thinking, hit **⏹ Cancel** and send again.

## For Developers (Building from source)

```bash
cargo run
```

The first build needs CMake on `PATH`. The Chrome TLS library compiles BoringSSL from source, and that step is the long one. Later builds reuse it. Run `cargo` from the repo root.

### Running Tests
```bash
cargo test
```

The fixture tests cover the page token, the request body, the length-prefixed stream, and the Safari and Firefox cookie readers. They do not call Gemini.

## macOS app

`./build_mac_app.sh` builds a double-clickable app. It does not install `cargo-bundle`. That crate's current release compiles hundreds of unrelated packages (code signing, AV1, image codecs) only to wrap a `.app`.

The script:

1. Builds only the `gemini_native_client` binary. For this script the release profile uses thin LTO and 8 codegen units, so the compile can finish on older CPUs. `Cargo.toml` still has full LTO and one codegen unit for a plain `cargo build --release`.
2. Assembles `target/release/bundle/osx/Gemini Native Client.app` with `assets/icon.icns`.

Install that bundle with:

```bash
cp -R "target/release/bundle/osx/Gemini Native Client.app" /Applications/
```

Rerun `./build_mac_app.sh` after changing the Rust sources, then copy the new bundle over the one in `/Applications`. Quit the app completely before opening the new copy. A running process keeps the binary it launched with.

## Cache locations in your home directory

`CACHE_DIR` = `~/.gemini_local`  
`COOKIE_FILE` = `~/.gemini_local/cookies.json`  
`PROFILE_DIR` = `~/.gemini_local/browser_profile`  
`WEBAPI_CACHE_DIR` = `~/.gemini_local/gemini_webapi_cache`  
`UI State File`: `~/Library/Application Support/Gemini-Native-Client/app.ron` (macOS) or `%APPDATA%/Gemini-Native-Client/app.ron` (Windows)


# Final Results

<img width="1920" height="1142" alt="Screenshot 2026-10-04 at 10 28 06 AM" src="https://github.com/user-attachments/assets/64a79a64-7c62-4f17-9b38-d6740fb0e86d" />

<img width="824" height="19" alt="Screenshot 2026-10-04 at 10 29 39 AM" src="https://github.com/user-attachments/assets/1716a469-3906-4f7f-acce-4cb5b0e5c6f9" />
<img width="767" height="21" alt="Screenshot 2026-10-04 at 10 29 01 AM" src="https://github.com/user-attachments/assets/7fb1906d-1173-48f2-aca2-a99908ec3414" />

Sign-in no longer downloads Playwright's Chromium. The window speaks the Gemini page protocol itself, with a Chrome 145 TLS fingerprint, so there is no Python helper to boot between messages.

