use acdd_core::config::AcddConfig;
use acdd_core::lint::{ArchRuleKind, LintCategory, LintSeverity};

#[test]
fn core_lint_contract_seam_bootstrap() {
    assert_eq!(LintCategory::ALL.len(), 6);
    assert_eq!(
        LintCategory::DEFAULT_VERIFY,
        [LintCategory::Arch, LintCategory::Syntax, LintCategory::Docs]
    );
    assert_eq!(ArchRuleKind::ALL.len(), 8);
    assert_eq!(LintSeverity::default(), LintSeverity::Error);

    let cfg = AcddConfig::default();
    assert!(cfg.architecture.is_empty());
}
