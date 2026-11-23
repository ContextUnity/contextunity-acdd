//! Deterministic test-only crash and failure injection hooks (`ACDD_FAILPOINT`).

/// Triggers a deterministic failpoint when `ACDD_FAILPOINT` matches `step`.
///
/// Compiled as a zero-cost no-op unless `cfg(test)` or the `failpoints` feature is enabled.
/// Supported `ACDD_FAILPOINT` syntax (comma-separated entries):
/// - `<step>` or `<step>:abort` — immediately terminates the process (exit code `134` via `_exit` on Unix) without unwinding, `Drop`s, or `SIGABRT` core dumps.
/// - `<step>:panic` — panics with `"failpoint hit: <step>"` for in-process `catch_unwind` tests.
/// - `<step>:exit` — exits immediately with code `137`.
#[cfg(any(test, feature = "failpoints"))]
fn abrupt_abort() -> ! {
    #[cfg(unix)]
    unsafe {
        // Terminate immediately without Rust Drop/atexit/stdio flush or SIGABRT core dumps.
        libc::_exit(134);
    }
    #[cfg(not(unix))]
    std::process::exit(134);
}

/// Evaluates `ACDD_FAILPOINT` unconditionally (used by failpoint builds and test harnesses).
pub fn hit_from_env(step: &str) {
    let Ok(spec) = std::env::var("ACDD_FAILPOINT") else {
        return;
    };
    for entry in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (target, action) = entry
            .split_once(':')
            .map_or((entry, "abort"), |(t, a)| (t.trim(), a.trim()));
        if target != step {
            continue;
        }
        match action {
            "panic" => panic!("failpoint hit: {step}"),
            "exit" => std::process::exit(137),
            _ => {
                #[cfg(any(test, feature = "failpoints"))]
                abrupt_abort();
                #[cfg(not(any(test, feature = "failpoints")))]
                {
                    #[cfg(unix)]
                    unsafe {
                        libc::_exit(134);
                    }
                    #[cfg(not(unix))]
                    std::process::exit(134);
                }
            }
        }
    }
}

#[cfg(any(test, feature = "failpoints"))]
pub fn hit(step: &str) {
    hit_from_env(step);
}

/// Zero-cost no-op failpoint hook when built without `failpoints`.
#[cfg(not(any(test, feature = "failpoints")))]
#[inline]
pub fn hit(_step: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failpoint_hit_ignores_unmatched_step() {
        // Without ACDD_FAILPOINT matching "never_hit_step", hit() returns cleanly.
        hit("never_hit_step");
    }
}
