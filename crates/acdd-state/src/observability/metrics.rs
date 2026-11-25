//! Pure Prometheus 0.0.4 text exposition formatter over [`ObservabilitySnapshot`].

use crate::types::ObservabilitySnapshot;
use std::fmt::Write;

/// Escape a Prometheus label value per the 0.0.4 text exposition specification.
fn escape_label_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Render [`ObservabilitySnapshot`] as Prometheus 0.0.4 exposition text.
pub fn render_prometheus_text(snapshot: &ObservabilitySnapshot) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "# HELP acdd_runs_active Number of active ACDD runs");
    let _ = writeln!(out, "# TYPE acdd_runs_active gauge");
    let _ = writeln!(out, "acdd_runs_active {}", snapshot.active_runs);

    let _ = writeln!(
        out,
        "# HELP acdd_runs_completed_total Total completed ACDD runs"
    );
    let _ = writeln!(out, "# TYPE acdd_runs_completed_total counter");
    let _ = writeln!(out, "acdd_runs_completed_total {}", snapshot.completed_runs);

    let _ = writeln!(
        out,
        "# HELP acdd_runs_cleared_total Total cleared ACDD runs"
    );
    let _ = writeln!(out, "# TYPE acdd_runs_cleared_total counter");
    let _ = writeln!(out, "acdd_runs_cleared_total {}", snapshot.cleared_runs);

    let _ = writeln!(out, "# HELP acdd_gate_nodes Current gate nodes by status");
    let _ = writeln!(out, "# TYPE acdd_gate_nodes gauge");
    for (status, count) in &snapshot.nodes_by_status {
        let _ = writeln!(
            out,
            "acdd_gate_nodes{{status=\"{}\"}} {}",
            escape_label_value(status),
            count
        );
    }

    let _ = writeln!(
        out,
        "# HELP acdd_gate_claims_active Number of currently active gate claims"
    );
    let _ = writeln!(out, "# TYPE acdd_gate_claims_active gauge");
    let _ = writeln!(out, "acdd_gate_claims_active {}", snapshot.active_claims);

    let _ = writeln!(
        out,
        "# HELP acdd_gate_traces_total Total gate traces by execution status"
    );
    let _ = writeln!(out, "# TYPE acdd_gate_traces_total counter");
    for (status, count) in &snapshot.traces_by_status {
        let _ = writeln!(
            out,
            "acdd_gate_traces_total{{status=\"{}\"}} {}",
            escape_label_value(status),
            count
        );
    }

    let _ = writeln!(
        out,
        "# HELP acdd_blackboard_open_items Number of open blackboard items"
    );
    let _ = writeln!(out, "# TYPE acdd_blackboard_open_items gauge");
    let _ = writeln!(
        out,
        "acdd_blackboard_open_items {}",
        snapshot.open_blackboard_items
    );

    out
}
