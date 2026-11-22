pub(super) fn dependencies(filename: &str, source: &str) -> Vec<String> {
    crate::languages::dependency_registry::parsers::python::extract_dependencies(filename, source)
}
