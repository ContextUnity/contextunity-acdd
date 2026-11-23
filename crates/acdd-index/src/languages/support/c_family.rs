use super::super::profile::LanguageProfile;
use super::super::types::{module_stem, relative_namespace, ImportPath, SyntaxContext};
use crate::ast::syntax::{self as ast, field, text};
use crate::models::Facts;
use tree_sitter::Node as Syntax;

pub(in crate::languages) fn symbol_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" => Some("function"),
        "struct_specifier" | "union_specifier" => Some("struct"),
        "enum_specifier" => Some("enum"),
        "type_definition" => Some("type"),
        _ => None,
    }
}
pub(in crate::languages) fn symbol(node: Syntax<'_>) -> Option<&'static str> {
    if matches!(
        node.kind(),
        "struct_specifier" | "union_specifier" | "enum_specifier"
    ) && node.child_by_field_name("body").is_none()
        && !node.parent().is_some_and(|p| {
            p.kind() == "declaration" && p.child_by_field_name("declarator").is_none()
        })
    {
        return None;
    }
    symbol_kind(node.kind())
}

pub(in crate::languages) fn symbol_name<'a>(node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name("name")
        .or_else(|| {
            node.child_by_field_name("declarator")
                .map(super::declarator)
        })
        .map(|n| text(n, source))
}
pub(in crate::languages) fn bindings(
    profile: &dyn LanguageProfile,
    node: Syntax<'_>,
    source: &str,
) -> ast::ScopeBindings {
    super::bindings(
        profile,
        node,
        source,
        &["parameter_declaration", "optional_parameter_declaration"],
        &["init_declarator", "declaration", "field_declaration"],
    )
}
pub(in crate::languages) fn include(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    if ctx.node.kind() == "preproc_include" {
        if let Some(path) = field(ctx.node, ctx.source, "path") {
            super::import(ctx, facts, path, Some("*".to_owned()));
        }
    }
}
fn strip_angle_header(module: &str) -> &str {
    module
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
}

pub(in crate::languages) fn is_c_stdlib(module: &str) -> bool {
    const HEADERS: &[&str] = &[
        "assert.h", "complex.h", "ctype.h", "errno.h", "fenv.h", "float.h", "inttypes.h",
        "iso646.h", "limits.h", "locale.h", "math.h", "setjmp.h", "signal.h", "stdalign.h",
        "stdarg.h", "stdatomic.h", "stdbool.h", "stddef.h", "stdint.h", "stdio.h", "stdlib.h",
        "stdnoreturn.h", "string.h", "tgmath.h", "threads.h", "time.h", "uchar.h", "wchar.h",
        "wctype.h", "unistd.h", "fcntl.h", "pthread.h", "dirent.h", "dlfcn.h", "netdb.h",
        "poll.h", "regex.h", "semaphore.h",
    ];
    let header = strip_angle_header(module);
    HEADERS.contains(&header)
        || header.starts_with("sys/")
        || header.starts_with("arpa/")
        || header.starts_with("netinet/")
}

#[cfg(feature = "lang-cpp")]
pub(in crate::languages) fn is_cpp_stdlib(module: &str) -> bool {
    const HEADERS: &[&str] = &[
        "algorithm", "any", "array", "atomic", "bitset", "cassert", "cctype", "cerrno", "cfloat",
        "chrono", "cinttypes", "climits", "cmath", "condition_variable", "cstddef", "cstdint",
        "cstdio", "cstdlib", "cstring", "ctime", "deque", "exception", "filesystem", "format",
        "forward_list", "fstream", "functional", "future", "initializer_list", "iomanip", "ios",
        "iosfwd", "iostream", "istream", "iterator", "limits", "list", "map", "memory", "mutex",
        "new", "numeric", "optional", "ostream", "queue", "random", "ranges", "ratio", "regex",
        "set", "shared_mutex", "span", "sstream", "stack", "stdexcept", "streambuf", "string",
        "string_view", "system_error", "thread", "tuple", "type_traits", "typeindex", "typeinfo",
        "unordered_map", "unordered_set", "utility", "variant", "vector",
    ];
    let header = strip_angle_header(module);
    is_c_stdlib(header) || HEADERS.contains(&header)
}

pub(in crate::languages) fn include_path(
    _profile: &dyn LanguageProfile,
    owner: &str,
    module: &str,
) -> Option<ImportPath> {
    let trimmed = module.trim();
    if let Some(header) = trimmed.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        let header = header.trim();
        return (!header.is_empty()).then(|| ImportPath::absolute(header.replace('/', ".")));
    }
    let path = trimmed.strip_prefix('"')?.strip_suffix('"')?;
    let stripped = module_stem(path);
    Some(ImportPath {
        namespace: relative_namespace(owner, stripped)?,
        relative: true,
        symbol_path: false,
    })
}
