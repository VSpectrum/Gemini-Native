//! The async runtime.
//!
//! The window thread is blocked inside the event loop, so the network needs
//! its own thread. One worker is enough: every Gemini call is waiting on
//! sockets, and the worker parks between polls. A worker per core would sit
//! idle and still cost a stack.
//!
//! Blocking work (the sign-in wait, writing a download) uses a separate pool
//! capped at two threads. The worker itself never calls `std::fs` or `sleep`,
//! so one pane's download cannot freeze another pane's reply.

pub const IO_WORKERS: usize = 1;
pub const BLOCKING_THREADS: usize = 2;

pub fn build() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(IO_WORKERS)
        .max_blocking_threads(BLOCKING_THREADS)
        .thread_name("gemini-io")
        .enable_io()
        .enable_time()
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_stays_small() {
        assert_eq!(IO_WORKERS, 1);
        assert_eq!(BLOCKING_THREADS, 2);
        let runtime = build().expect("runtime");
        runtime.block_on(async {
            let (first, second) = tokio::join!(
                async { 1 },
                async { 2 },
            );
            assert_eq!(first + second, 3);
        });
    }
}
