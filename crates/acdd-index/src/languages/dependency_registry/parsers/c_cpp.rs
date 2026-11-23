use super::super::parser::{Ecosystem, ManifestParser, ParsedManifest};

/// Parser for C/C++ build manifests (`CMakeLists.txt`, `meson.build`).
pub(crate) struct CCppParser;

/// Shared C/C++ manifest parser instance.
pub(crate) static C_CPP_PARSER: CCppParser = CCppParser;

impl ManifestParser for CCppParser {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::CCpp
    }

    fn filenames(&self) -> &'static [&'static str] {
        &["CMakeLists.txt", "meson.build"]
    }

    fn parse(&self, filename: &str, content: &str) -> ParsedManifest {
        let mut parsed = ParsedManifest::new(Ecosystem::CCpp);
        if filename == "meson.build" {
            parse_meson_dependencies(content, &mut parsed.dependencies);
        } else {
            parse_cmake_dependencies(content, &mut parsed.dependencies);
        }
        parsed.finish()
    }
}

fn parse_cmake_dependencies(content: &str, out: &mut Vec<String>) {
    let cleaned: String = content
        .lines()
        .map(|line| line.split('#').next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let lower = cleaned.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(rel) = lower[offset..].find("find_package") {
        let pos = offset + rel;
        let before = cleaned[..pos].chars().next_back();
        let after_start = pos + "find_package".len();
        offset = after_start;
        if before.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            continue;
        }
        let rest = cleaned[after_start..].trim_start();
        let Some(inner) = rest.strip_prefix('(') else {
            continue;
        };
        let Some(end) = inner.find(')') else {
            break;
        };
        if let Some(pkg) = inner[..end]
            .split_whitespace()
            .next()
            .map(|token| token.trim_matches(['"', '\'']))
            .filter(|token| !token.is_empty() && !token.starts_with("${"))
        {
            out.push(pkg.to_owned());
        }
    }
}

fn parse_meson_dependencies(content: &str, out: &mut Vec<String>) {
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or(raw);
        let mut rest = line;
        while let Some(pos) = rest.find("dependency(") {
            let before = rest[..pos].chars().next_back();
            let after = &rest[pos + "dependency(".len()..];
            rest = after;
            if before.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                continue;
            }
            let arg = after.trim_start();
            let Some(quote) = arg.chars().next().filter(|ch| matches!(ch, '\'' | '"')) else {
                continue;
            };
            let body = &arg[quote.len_utf8()..];
            if let Some(end) = body.find(quote) {
                let pkg = body[..end].trim();
                if !pkg.is_empty() {
                    out.push(pkg.to_owned());
                }
            }
        }
    }
}
