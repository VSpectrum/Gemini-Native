use std::time::{Duration, Instant};

use gemini_native_client::runtime;

#[test]
fn io_tasks_overlap_on_the_runtime() {
    let runtime = runtime::build().expect("runtime");
    runtime.block_on(async {
        let started = Instant::now();
        let slow = async {
            tokio::time::sleep(Duration::from_millis(250)).await;
            started.elapsed()
        };
        let fast = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            started.elapsed()
        };
        let (slow_at, fast_at) = tokio::join!(slow, fast);
        assert!(
            fast_at < Duration::from_millis(160),
            "fast task waited behind the slow one: {fast_at:?}"
        );
        assert!(slow_at >= Duration::from_millis(200), "{slow_at:?}");
        assert!(fast_at < slow_at);
    });
}

#[test]
fn blocking_work_does_not_stall_the_io_worker() {
    let runtime = runtime::build().expect("runtime");
    runtime.block_on(async {
        let started = Instant::now();
        let blocking = tokio::task::spawn_blocking(|| {
            std::thread::sleep(Duration::from_millis(300));
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(180),
            "the io worker waited on blocking work: {elapsed:?}"
        );
        blocking.await.expect("blocking task");
    });
}
