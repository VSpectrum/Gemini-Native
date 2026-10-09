//! Opt-in CPU / RAM monitor.
//!
//! Designed so that it costs *nothing* while disabled: no thread, no `sysinfo::System`, no
//! sample buffers. Enabling it spawns a single background thread that samples at the chosen
//! interval and asks egui for exactly one repaint per sample.

use eframe::egui;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// User-facing settings. Persisted with the rest of the app state.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct MonitorSettings {
    pub enabled: bool,
    pub interval_secs: u64,
    /// Also measure child processes (Python helper, Playwright driver, Chromium).
    /// Requires a scan of the full process table, so it is noticeably more expensive.
    pub include_helpers: bool,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: 2,
            include_helpers: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    /// Percent of one core (can exceed 100% on multi-core machines, like Activity Monitor/top).
    pub app_cpu: f32,
    pub app_rss: u64,
    pub helper_cpu: f32,
    pub helper_rss: u64,
    pub helper_count: usize,
}

#[derive(Default)]
struct Shared {
    latest: Option<Sample>,
    peak_rss: u64,
}

struct Config {
    stop: AtomicBool,
    interval_ms: AtomicU64,
    include_helpers: AtomicBool,
}

/// Running monitor. Dropping it stops the sampling thread.
pub struct ResourceMonitor {
    shared: Arc<Mutex<Shared>>,
    config: Arc<Config>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ResourceMonitor {
    pub fn start(ctx: egui::Context, settings: &MonitorSettings) -> Option<Self> {
        let pid = sysinfo::get_current_pid().ok()?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let config = Arc::new(Config {
            stop: AtomicBool::new(false),
            interval_ms: AtomicU64::new(settings.interval_secs.max(1) * 1000),
            include_helpers: AtomicBool::new(settings.include_helpers),
        });

        let thread = {
            let shared = Arc::clone(&shared);
            let config = Arc::clone(&config);
            std::thread::Builder::new()
                .name("resource-monitor".into())
                .spawn(move || sample_loop(pid, ctx, shared, config))
                .ok()?
        };

        Some(Self {
            shared,
            config,
            thread: Some(thread),
        })
    }

    /// Pushes changed settings to the running thread without restarting it.
    pub fn apply(&self, settings: &MonitorSettings) {
        self.config
            .interval_ms
            .store(settings.interval_secs.max(1) * 1000, Ordering::Relaxed);
        self.config
            .include_helpers
            .store(settings.include_helpers, Ordering::Relaxed);
        if let Some(t) = &self.thread {
            t.thread().unpark(); // take a fresh sample right away
        }
    }

    pub fn ui(&self, ui: &mut egui::Ui, settings: &mut MonitorSettings) {
        // Copy what we need out of the lock so painting never blocks the sampler.
        let (latest, peak) = {
            let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.latest,
                s.peak_rss,
            )
        };

        ui.horizontal(|ui| {
            ui.strong("📊 Resources");
            ui.separator();

            match latest {
                None => {
                    ui.weak("Collecting first sample…");
                }
                Some(s) => {
                    let cores = std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(1) as f32;
                    ui.label(format!("CPU {:.1}%", s.app_cpu)).on_hover_text(format!(
                        "UI process CPU, where 100% = one full core.\n≈ {:.1}% of total machine capacity ({} logical cores).",
                        s.app_cpu / cores,
                        cores
                    ));

                    ui.separator();
                    ui.label(format!("RAM {}", fmt_bytes(s.app_rss)))
                        .on_hover_text(format!(
                            "Resident memory of the UI process.\nPeak this session: {}",
                            fmt_bytes(peak)
                        ));

                    if settings.include_helpers {
                        ui.separator();
                        if s.helper_count == 0 {
                            ui.weak("Helpers: idle");
                        } else {
                            ui.label(format!(
                                "Helpers ({}): CPU {:.1}% · RAM {}",
                                s.helper_count,
                                s.helper_cpu,
                                fmt_bytes(s.helper_rss)
                            ))
                            .on_hover_text(
                                "Child processes spawned by the app (Python bridge, Playwright, Chromium).",
                            );
                        }
                    }
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✖").on_hover_text("Disable monitor").clicked() {
                    settings.enabled = false;
                }
                ui.checkbox(&mut settings.include_helpers, "Helpers")
                    .on_hover_text(
                        "Also measure child processes. Scans the full process table each sample, so it costs more CPU.",
                    );
                egui::ComboBox::from_id_source("monitor_interval")
                    .width(50.0)
                    .selected_text(format!("{}s", settings.interval_secs))
                    .show_ui(ui, |ui| {
                        for secs in [1, 2, 5, 10] {
                            ui.selectable_value(&mut settings.interval_secs, secs, format!("{secs}s"));
                        }
                    })
                    .response
                    .on_hover_text("Sampling interval. Longer = less overhead.");
            });
        });
    }
}

impl Drop for ResourceMonitor {
    fn drop(&mut self) {
        self.config.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            // Wake it so it exits promptly; don't join, to never stall the UI thread.
            t.thread().unpark();
        }
    }
}

fn sample_loop(pid: Pid, ctx: egui::Context, shared: Arc<Mutex<Shared>>, config: Arc<Config>) {
    let mut sys = System::new();
    let kind = ProcessRefreshKind::nothing().with_cpu().with_memory();

    // Prime CPU counters: usage is computed from the delta between two refreshes.
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
    let mut had_helpers_scan = false;

    loop {
        std::thread::park_timeout(Duration::from_millis(
            config.interval_ms.load(Ordering::Relaxed),
        ));
        if config.stop.load(Ordering::Relaxed) {
            return;
        }

        let include_helpers = config.include_helpers.load(Ordering::Relaxed);
        if include_helpers {
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
            had_helpers_scan = true;
        } else {
            if had_helpers_scan {
                // Release the full process table we no longer need.
                sys = System::new();
                had_helpers_scan = false;
            }
            sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
        }

        let mut sample = Sample::default();
        if let Some(p) = sys.process(pid) {
            sample.app_cpu = p.cpu_usage();
            sample.app_rss = p.memory();
        }

        if include_helpers {
            for proc_ in sys.processes().values() {
                if is_descendant(&sys, proc_.pid(), pid) {
                    sample.helper_cpu += proc_.cpu_usage();
                    sample.helper_rss += proc_.memory();
                    sample.helper_count += 1;
                }
            }
        }

        {
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
            s.latest = Some(sample);
            s.peak_rss = s.peak_rss.max(sample.app_rss);
        }

        ctx.request_repaint();
    }
}

fn is_descendant(sys: &System, mut pid: Pid, ancestor: Pid) -> bool {
    // Bounded walk guards against PID-reuse cycles.
    for _ in 0..32 {
        match sys.process(pid).and_then(|p| p.parent()) {
            Some(parent) if parent == ancestor => return true,
            Some(parent) if parent != pid => pid = parent,
            _ => return false,
        }
    }
    false
}



pub fn fmt_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else {
        format!("{mb:.1} MB")
    }
}



#[cfg(test)]
mod tests {
    use super::*;



    #[test]
    fn fmt_bytes_units() {
        assert_eq!(fmt_bytes(50 * 1024 * 1024), "50.0 MB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn default_is_disabled() {
        assert!(!MonitorSettings::default().enabled);
    }
}
