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

If you are downloading a pre-built release for your operating system, the Python scripts and dependencies are bundled natively. No Python or pip is required. Sign-in opens a browser you already have installed (Chrome, Edge, Brave, Chromium, or Firefox). Nothing downloads a second Chromium.

## 1. Run the App & Get Cookies
1. Open the downloaded `Gemini Native Client` app (or `.exe` on Windows).
2. Type any simple message (e.g., "Hello") and press Enter.
3. The app will detect you don't have cookies yet and pop open a private window in a browser that is already installed.
4. **Log in to your Google Account** in that window. It uses its own profile, so this does not touch the cookies in your everyday browser.
5. Once logged in, the window verifies that both session cookies (`__Secure-1PSID` and `__Secure-1PSIDTS`) are present, closes itself, and passes control back to the native Rust UI.

You can skip the window by setting `GEMINI_PSID` and `GEMINI_PSIDTS` yourself (the same two cookie values, from the browser's cookie store).

## Key Features & Stability
- **Resilient Dual-Cookie Authentication**: Specifically verifies both `__Secure-1PSID` and timestamped `__Secure-1PSIDTS` cookies before concluding sign-in, preventing silent auth drops or endless "Thinking (or checking cookies)" hangs.
- **In-Flight Request Cancellation**: Every active request displays a **⏹ Cancel** button right next to the busy indicator, letting you abort a stuck or slow prompt and instantly terminate the helper process.
- **Timeouts**: Sign-in initialization is capped at 30 seconds. `GeminiClient.init` is given a 120 second timeout because `gemini_webapi` stores that value and reuses it as the stream stall limit. A 15 second init timeout makes generation abandon a live reply after 20 seconds. The helper then waits up to 120 seconds for `generate_content`, and the Rust UI kills the helper after 5 minutes.
- **One-Click Session Reset**: Hit **🗑 Clear Session** in the top menu to wipe stored session cookies and isolated API caches if credentials ever need a refresh.
- **Automatic Task Teardown**: Closing or dropping a conversation pane terminates any linked background Python processes (`kill_on_drop`), eliminating zombie helper processes.

## Sending a message

The desktop window shows a reply when the helper prints one JSON line. The first reply after login often takes about half a minute: session init, then generation. Later messages in that pane reuse the same helper, so they skip process startup and session init. The helper exits after five minutes with nothing to do.

If a pane stays on Thinking, hit **⏹ Cancel** and send again. Cancel terminates that helper. A later send starts a new one.

## For Developers (Building from source)

There is no `requirements.txt`. From the repo:

```bash
python3 -m venv venv
source venv/bin/activate
pip install gemini_webapi loguru pillow
cargo run
```

`cargo run` launches `python3 gemini_auto.py --serve` from the working directory. Run it from the repo root. The bundled `.app` does not use that `python3`. It runs `gemini_helper` inside `Contents/MacOS`.

### Running Tests
Execute the full test suite (Rust UI/engine tests and Python auth/cookie tests):
```bash
cargo test
python3 -m unittest discover -s tests
```

The Python tests only need the standard library. Use the virtualenv when you want to exercise `gemini_webapi` itself.

## macOS app

`./build_mac_app.sh` builds a double-clickable app. It does not install `cargo-bundle`. That crate's current release compiles hundreds of unrelated packages (code signing, AV1, image codecs) only to wrap a `.app`.

The script:

1. Creates `venv` on first run and installs `pyinstaller`, `gemini_webapi`, `loguru`, and `pillow`.
2. Rebuilds `dist/gemini_helper` (one directory, not two onefile binaries) when a helper source file is newer than the binary.
3. Builds only the `gemini_native_client` binary. For this script the release profile uses thin LTO and 8 codegen units, so the compile can finish on older CPUs. `Cargo.toml` still has full LTO and one codegen unit for a plain `cargo build --release`.
4. Assembles `target/release/bundle/osx/Gemini Native Client.app` with `assets/icon.icns` and copies `gemini_helper` into `Contents/MacOS`.

Install that bundle with:

```bash
cp -R "target/release/bundle/osx/Gemini Native Client.app" /Applications/
```

Rerun `./build_mac_app.sh` after changing `gemini_auto.py`, `gemini_download.py`, `browser_session.py`, `gemini_helper.py`, or the Rust UI, then copy the new bundle over the one in `/Applications`. The installed app keeps using whatever helper is inside its own `Contents/MacOS`. When only the helper changed, `cargo` reuses the existing `gemini_native_client` binary and the script still rebuilds `gemini_helper`.

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

Sign-in no longer downloads Playwright's Chromium. The helper is one directory instead of two packed Python binaries, and a pane keeps that process warm so the next message does not boot it again. The UI still is not a 20MB TUI. Markdown and the Gemini client are the pieces that remain.

