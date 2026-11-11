# Gemini Native Client Optimizations

This document tracks the performance optimizations implemented to ensure the application uses as little CPU and RAM as possible.

## 1. Markdown Cache Sharing
- **What was optimized:** `egui_commonmark` markdown parsing cache.
- **Why it was optimized:** Previously, every single chat tab (`Pane`) instantiated its own `CommonMarkCache`. Each cache instance parses and stores heavy syntax highlighting themes and structs in memory. With multiple panes open, RAM usage scaled exponentially and caused severe bloat.
- **How it was changed:** The architecture was refactored so that a single `CommonMarkCache` is instantiated inside the main `GeminiApp`. This shared cache is then passed by reference to `TreeBehavior` which distributes it to all panes during rendering.

## 2. Off-Screen Message Rendering Cache
- **What was optimized:** Layout generation for long conversation histories.
- **Why it was optimized:** `egui` is an immediate mode GUI, meaning it calculates the height and layout of every message in a conversation on every single frame, even if those messages are scrolled far off-screen. This spiked CPU usage in long chats.
- **How it was changed:** Implemented `MsgRenderCache` inside `Pane`. Once a message is rendered and its height is determined, the height is cached. If the message scrolls out of the visible area, it is replaced with a pre-calculated empty `Spacer` widget. This completely bypasses the markdown parsing and layout calculations for off-screen text.

## 3. Custom Tokio Runtime
- **What was optimized:** The asynchronous runtime engine.
- **Why it was optimized:** The app previously relied on the default `#[tokio::main]` macro, which spins up a heavy multi-threaded worker pool (one thread per CPU core). Since the app's async tasks only consisted of waiting for child processes and basic I/O, these threads were dormant but still consumed base memory.
- **How it was changed:** The `#[tokio::main]` macro was removed. `src/main.rs` builds a multi-thread runtime with one worker, named `gemini-io`, then calls `runtime.enter()` so the UI thread can `tokio::spawn`. The UI thread stays inside the winit loop. The worker is what actually awaits helper processes. A current-thread runtime cannot do that, because nothing would drive it while the event loop is blocked.

## 4. UI Repaint Rate (Spinner Removal)
- **What was optimized:** The visual loading indicator in the panes.
- **Why it was optimized:** The default `egui::Spinner` widget forces the application to continuously repaint at 60 FPS in order to smoothly animate its rotation. This caused high idle CPU usage whenever the app was waiting for an AI response.
- **How it was changed:** Replaced the continuous spinner with a manual text-based `activity_label` (e.g. cycling through `-`, `\`, `|`, `/`). The label manually requests a repaint exactly twice a second (2 Hz), ensuring the CPU idles beautifully while waiting.

## 5. Infinite Repaint Loop Fix
- **What was optimized:** The `ResourceMonitor` state synchronization.
- **Why it was optimized:** When the resource monitor was enabled, the app's CPU spiked to 75%+. This was because `monitor.apply()` was being called on every frame, which forcefully woke up the background monitoring thread. The thread would sample, request a UI repaint, and go back to sleep. But the resulting repaint triggered `apply()` again, creating an infinite spin-loop.
- **How it was changed:** Added a check to compare the `MonitorSettings` before and after the UI is drawn. The background thread is now only forcibly woken up if a setting (like the refresh interval) actually changes.

## 6. Lightweight Resource Monitor
- **What was optimized:** The background resource tracking system.
- **Why it was optimized:** To satisfy user preference and improve performance, tracking memory/CPU arrays over time was unnecessary if the user only wanted current values.
- **How it was changed:** Removed the `VecDeque` arrays and bounded history logic used for sparkline graphs. The monitoring thread now only maintains the `latest` sample, practically eliminating its memory footprint.

## 7. Cargo Release Profile Tuning
- **What was optimized:** `Cargo.toml` compilation settings.
- **Why it was optimized:** The default release settings compile relatively quickly but don't heavily compress the binary or optimize code paths across dependencies.
- **How it was changed:** Added the following flags to `[profile.release]`:
  - `lto = true`: Link Time Optimization allows the compiler to optimize across crate boundaries.
  - `codegen-units = 1`: Forces the compiler to analyze the entire program at once, finding more optimization opportunities.
  - `strip = true`: Removes debugging symbols, massively reducing binary file size.
- `./build_mac_app.sh` overrides that profile for the `.app` only: thin LTO and 8 codegen units (`--config profile.release.lto="thin"` and `profile.release.codegen-units=8`). Full LTO on one codegen unit is what makes a release compile sit for a long time on older CPUs. A plain `cargo build --release` still uses the `Cargo.toml` profile.

## 8. Dependency Pruning
- **What was optimized:** Feature flags for third-party libraries.
- **Why it was optimized:** Many crates pull in large trees of features by default. For instance, `image` supports dozens of formats and `tokio` includes time, process, and synchronization modules.
- **How it was changed:** Disabled `default-features` for large dependencies. `tokio` enables `rt-multi-thread`, `process`, `time`, `io-util`, and `sync` (the last two keep a warm helper's pipes and channel). `image` enables `png`, `jpeg`, `gif`, and `webp`. `sysinfo` enables `system` and leaves its default `multithread` feature off.

## 9. Background Subprocess Consoles
- **What was optimized:** Terminal window spawning on Windows OS.
- **Why it was optimized:** On Windows, running child helper processes (like Python or Playwright) would sometimes spawn detached `cmd.exe` windows, which caused overhead and cluttered the taskbar.
- **How it was changed:** Release Windows builds set `windows_subsystem = "windows"` so the UI has no host console. `helper_command` in `src/pane.rs` passes `CREATE_NO_WINDOW` on the `tokio::process::Command` for `gemini_auto` and `gemini_download`. Debug builds keep the console.

## 10. No Playwright, no private Chromium
- **What was optimized:** Session-cookie collection, install size, and first-run download.
- **Why it was optimized:** Every message imported Playwright, and the first sign-in downloaded a private Chromium (about 150MB) plus Playwright's driver. That dominated install size and the time-to-first-prompt on the machines this app is for.
- **How it was changed:** `browser_session.py` opens a browser that is already installed (Chrome, Edge, Brave, Chromium, or Firefox) with a profile under `~/.gemini_local/browser_profile`. Chromium-family browsers are read through a small DevTools client in the standard library, which returns decrypted cookies. Firefox's cookie database is already plaintext. Those everyday profiles are not read: rotating `__Secure-1PSIDTS` out of a shared profile can sign that browser out. Safari has no separate profile flag, so its cookie file is copied out and never written back. `GEMINI_PSID` and `GEMINI_PSIDTS` skip the window entirely.

## 11. One warm helper, one bundle
- **What was optimized:** Per-message process startup, and shipping two copies of Python.
- **Why it was optimized:** PyInstaller `--onefile` unpacks the interpreter into a temp directory on every launch, and `gemini_auto` / `gemini_download` each carried their own copy. `GeminiClient.init` also ran again for every prompt.
- **How it was changed:** The app bundles one `--onedir` `gemini_helper` (`serve`, `prompt`, or `download`). A pane keeps `gemini_auto.py --serve` alive and writes one JSON request per line. The process exits after five idle minutes. Cancel and Clear Session kill the process group, and a detached watcher stops a sign-in browser if Chrome has detached from that group.

## 12. Drop eframe features the UI does not use
- **What was optimized:** Binary size and compile time of the native window.
- **Why it was optimized:** eframe's default features link AccessKit and winit's Wayland client-side decoration stack. AccessKit is a screen-reader backend this app does not drive. The Wayland decoration crates are unused on macOS.
- **How it was changed:** `eframe` is built with `default-features = false` and only `default_fonts`, `glow`, `persistence`, `x11`, and `wayland`. x11 and wayland stay so a Linux build still opens a window; their native dependencies are target-gated and are not compiled into the macOS binary. `egui_commonmark` does not enable `better_syntax_highlighting`. That feature embeds syntect's full syntax and theme dumps. The crate has no `syntect` feature; the previous flag did not turn highlighting on. Code blocks use egui's built-in lexer.

## 13. In-repo Gemini protocol
- **What was optimized:** The Python dependency set behind every prompt.
- **Why it was optimized:** `gemini_webapi` pulled in curl_cffi plus its own model, gem, upload, and research surface. The app only needs a signed-in page, one StreamGenerate call, and the reply text.
- **How it was changed:** `gemini_protocol.py` performs that slice with curl_cffi's Chrome impersonation. Fixture tests cover the page token, the request body, and the length-prefixed stream. A live test sends one prompt when saved cookies are present. `gemini_auto.py` and `gemini_download.py` use this client, so the helper no longer imports `gemini_webapi`, loguru, or pillow.
