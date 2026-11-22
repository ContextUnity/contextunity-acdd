pub(super) fn extract_dependencies(filename: &str, content: &str) -> Vec<String> {
    crate::languages::dependency_registry::parsers::npm::extract_dependencies(filename, content)
}
