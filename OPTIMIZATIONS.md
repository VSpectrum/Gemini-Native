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
- **How it was changed:** The `#[tokio::main]` macro was removed. `src/runtime.rs` builds a multi-thread runtime with one worker, named `gemini-io`, then `main` calls `runtime.enter()` so the UI thread can `tokio::spawn`. The UI thread stays inside the winit loop. The worker polls every pane's task, so conversations run concurrently without a thread per core. Short blocking calls use a pool of two threads. Sign-in polling sleeps on the worker with `tokio::time::sleep`, so the wait between cookie reads does not occupy that pool.

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
- **How it was changed:** Disabled `default-features` for large dependencies. `tokio` enables `rt-multi-thread`, `macros`, `time`, `io-util`, `sync`, and `net`. `image` enables `png`, `jpeg`, `gif`, and `webp`. `sysinfo` enables `system` and leaves its default `multithread` feature off.

## 9. Background Subprocess Consoles
- **What was optimized:** Terminal window spawning on Windows OS.
- **Why it was optimized:** On Windows, starting a sign-in browser would sometimes leave a console window on the taskbar.
- **How it was changed:** Release Windows builds set `windows_subsystem = "windows"` so the UI has no host console. Debug builds keep the console. A sign-in browser is started with `CREATE_NO_WINDOW` on Windows.

## 10. No Playwright, no private Chromium
- **What was optimized:** Session-cookie collection, install size, and first-run download.
- **Why it was optimized:** Every message imported Playwright, and the first sign-in downloaded a private Chromium (about 150MB) plus Playwright's driver. That dominated install size and the time-to-first-prompt on the machines this app is for.
- **How it was changed:** `src/browser/` opens a browser that is already installed (Chrome, Edge, Brave, Chromium, or Firefox) with a profile under `~/.gemini_local/browser_profile`. Chromium-family browsers are read through a small DevTools client, which returns decrypted cookies. Firefox's cookie database is already plaintext. Those everyday profiles are not read: rotating `__Secure-1PSIDTS` out of a shared profile can sign that browser out. Safari has no separate profile flag, so its cookie file is read in place and never written back. `GEMINI_PSID` and `GEMINI_PSIDTS` skip the window entirely. The wait between reads is an async sleep, so the sign-in does not occupy a blocking thread.

## 11. The Gemini client lives in the window process
- **What was optimized:** A second runtime and a per-pane helper process.
- **Why it was optimized:** The window is Rust. The helper was a bundled CPython plus `curl_cffi`, kept warm so the next message did not boot an interpreter. That process was most of the install size and a resident cost on the old machines this app is for.
- **How it was changed:** Sign-in, StreamGenerate, quota, and media download run in the window process. `wreq` with the Chrome 145 emulation profile replaces `curl_cffi`. Cancel and Clear Session drop that client and, while a sign-in browser is open, signal its process group. A detached watcher still stops the browser if the app dies and Chrome has detached.

## 12. Drop eframe features the UI does not use
- **What was optimized:** Binary size and compile time of the native window.
- **Why it was optimized:** eframe's default features link AccessKit and winit's Wayland client-side decoration stack. AccessKit is a screen-reader backend this app does not drive. The Wayland decoration crates are unused on macOS.
- **How it was changed:** `eframe` is built with `default-features = false` and only `default_fonts`, `glow`, `persistence`, `x11`, and `wayland`. x11 and wayland stay so a Linux build still opens a window; their native dependencies are target-gated and are not compiled into the macOS binary. `egui_commonmark` does not enable `better_syntax_highlighting`. That feature embeds syntect's full syntax and theme dumps. The crate has no `syntect` feature; the previous flag did not turn highlighting on. Code blocks use egui's built-in lexer.

## 13. In-repo Gemini protocol
- **What was optimized:** The request the app sends to Gemini.
- **Why it was optimized:** `gemini_webapi` pulled in curl_cffi plus its own model, gem, upload, and research surface. The app only needs a signed-in page, one StreamGenerate call, and the reply text.
- **How it was changed:** `src/protocol.rs` and `src/gemini.rs` perform that slice. Fixture tests cover the page token, the request body, and the length-prefixed stream. A model-tagged request that stalls is retried once without the model header, which is the same failure the old library hit on that header.

## 14. Sign-in wait yields the worker
- **What was optimized:** The five-minute cookie poll.
- **Why it was optimized:** The poll used to `thread::sleep` inside `spawn_blocking`, so one of the two blocking threads was occupied for the whole sign-in. A download in another pane then had to wait.
- **How it was changed:** `poll_for_cookies` sleeps with `tokio::time::sleep`. Each cookie read, sqlite query, and process spawn is its own short `spawn_blocking` call. Two polls are covered by a unit test that deadlocks if they stop overlapping.

## 15. Closing a folder
- **What was optimized:** Marking every pane in a folder closed.
- **Why it was optimized:** The old loop searched the tile tree once per conversation.
- **How it was changed:** `GeminiApp::close_folder` collects the folder's ids, then walks the open panes once.

## 16. Clear Session stays off the UI thread
- **What was optimized:** Deleting the sign-in profile.
- **Why it was optimized:** `remove_dir_all` on the UI thread froze the window while a browser profile was removed.
- **How it was changed:** The button drops in-memory clients immediately and deletes the cookie file and profile on the blocking pool. A generation counter makes a sign-in that was already in flight skip its save if Clear Session won the race.
