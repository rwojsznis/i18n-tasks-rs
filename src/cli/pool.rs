//! The rayon pool the source scan fans out over.

/// Sizes the pool. Called once, from `Common::open`, so `--jobs` belongs to the
/// commands that read a project and not to `migrate-config`, which scans
/// nothing.
///
/// Without `--jobs`, `rayon` uses the core count.
pub(crate) fn install_pool(jobs: Option<usize>) -> Result<(), String> {
    let mut builder = rayon::ThreadPoolBuilder::new();
    if let Some(n) = jobs {
        if n == 0 {
            return Err("--jobs must be at least 1".to_string());
        }
        builder = builder.num_threads(n);
    }
    // The Prism visitor recurses over the AST, and a worker thread's default
    // stack is a quarter of the main thread's. Match the main thread instead of
    // failing on one deeply nested file.
    builder
        .stack_size(8 * 1024 * 1024)
        .build_global()
        .map_err(|e| match jobs {
            Some(n) => format!("cannot start {n} worker threads: {e}"),
            // Without `--jobs` the count is rayon's, not ours, so name no
            // number rather than a made-up one.
            None => format!("cannot start the worker thread pool: {e}"),
        })
}

#[cfg(test)]
mod tests {
    use super::install_pool;

    /// The second `build_global` in a process always fails, which is the only
    /// way to reach the error arm without a resource limit. Keep this the only
    /// test in the binary that touches the pool: the tests share one process,
    /// so a second one would see whichever ran first.
    #[test]
    fn pool_error_does_not_invent_a_thread_count() {
        install_pool(Some(2)).expect("first install builds the global pool");
        let err = install_pool(None).expect_err("second install must fail");
        assert!(
            !err.contains('0'),
            "no --jobs, so the message must not name a thread count: {err}"
        );
        let err = install_pool(Some(4)).expect_err("second install must fail");
        assert!(
            err.contains("4 worker threads"),
            "with --jobs the message names the asked-for count: {err}"
        );
    }
}
