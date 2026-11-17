//! Pure run closure digests, metrics rollups, and Prometheus 0.0.4 exposition formatting.

pub mod digest;
pub mod metrics;

#[cfg(test)]
mod tests;

pub use digest::build_completed_run_digest;
pub use metrics::render_prometheus_text;
