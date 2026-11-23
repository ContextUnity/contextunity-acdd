/// Performs kind.
pub fn kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration"
        | "generator_function_declaration"
        | "generator_function"
        | "generator_function_expression"
        | "arrow_function"
        | "function_expression" => Some("function"),
        "method_definition" | "method_signature" => Some("method"),
        "class_declaration" | "class" => Some("class"),
        "enum_declaration" => Some("enum"),
        "enum_assignment" => Some("field"),
        "interface_declaration" => Some("interface"),
        "field_definition" => Some("field"),
        "type_alias_declaration" => Some("type"),
        _ => None,
    }
}
