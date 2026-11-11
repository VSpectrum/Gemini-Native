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
- **How it was changed:** Disabled `default-features` for large dependencies. `tokio` enables `rt-multi-thread`, `process`, and `time`. `image` enables `png`, `jpeg`, `gif`, and `webp`. `sysinfo` enables `system` and leaves its default `multithread` feature off.

## 9. Background Subprocess Consoles
- **What was optimized:** Terminal window spawning on Windows OS.
- **Why it was optimized:** On Windows, running child helper processes (like Python or Playwright) would sometimes spawn detached `cmd.exe` windows, which caused overhead and cluttered the taskbar.
- **How it was changed:** Release Windows builds set `windows_subsystem = "windows"` so the UI has no host console. `helper_command` in `src/pane.rs` passes `CREATE_NO_WINDOW` on the `tokio::process::Command` for `gemini_auto` and `gemini_download`. Debug builds keep the console.
