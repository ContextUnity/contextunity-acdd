use acdd_core::ports::{is_builtin_mcp_tool, BUILTIN_MCP_TOOLS};
use acdd_core::subsystems::{SubsystemId, SubsystemRegistry};

#[test]
fn mcp_lint_surface_seam_bootstrap() {
    assert_eq!(BUILTIN_MCP_TOOLS.len(), 20);
    assert!(is_builtin_mcp_tool("code_map_lint"));
    assert_eq!(
        SubsystemRegistry::subsystem_id_for_tool("code_map_lint"),
        Some(SubsystemId::Code)
    );
}
