use super::super::parser::{Ecosystem, ManifestParser, ModulePrefix, ParsedManifest};

/// Parser for C# MSBuild `*.csproj` manifests (`PackageReference` and `RootNamespace`).
pub(crate) struct CSharpParser;

/// Shared C# manifest parser instance.
pub(crate) static CSHARP_PARSER: CSharpParser = CSharpParser;

impl ManifestParser for CSharpParser {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::CSharp
    }

    fn filenames(&self) -> &'static [&'static str] {
        &["*.csproj"]
    }

    fn parse(&self, _filename: &str, content: &str) -> ParsedManifest {
        let mut parsed = ParsedManifest::new(Ecosystem::CSharp);
        let stripped = strip_xml_comments(content);

        if let Some(ns) = extract_xml_tag(&stripped, "RootNamespace")
            .or_else(|| extract_xml_tag(&stripped, "AssemblyName"))
        {
            if !ns.is_empty() && !ns.chars().any(char::is_whitespace) {
                parsed.own_packages.push(ns.clone());
                parsed.module_prefixes.push(ModulePrefix {
                    ecosystem: Ecosystem::CSharp,
                    prefix: ns,
                    source_root: String::new(),
                    origin: String::new(),
                });
            }
        }

        let mut search_from = 0;
        while let Some(rel) = stripped[search_from..].find("<PackageReference") {
            let start = search_from + rel + "<PackageReference".len();
            if stripped
                .as_bytes()
                .get(start)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b'>' | b'/'))
            {
                search_from = start;
                continue;
            }
            let Some(tag_end_rel) = stripped[start..].find('>') else {
                break;
            };
            let attrs = &stripped[start..start + tag_end_rel];
            if let Some(pkg) = xml_attr(attrs, "Include").or_else(|| xml_attr(attrs, "Update")) {
                if !pkg.is_empty() && !pkg.starts_with("$(") {
                    parsed.dependencies.push(pkg.to_owned());
                }
            }
            search_from = start + tag_end_rel + 1;
        }

        parsed.finish()
    }
}

fn strip_xml_comments(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        if let Some(end) = after.find("-->") {
            rest = &after[end + 3..];
        } else {
            return out;
        }
    }
    out.push_str(rest);
    out
}

fn extract_xml_tag(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)? + start;
    let value = content[start..end].trim();
    (!value.is_empty() && !value.starts_with("$(")).then(|| value.to_owned())
}

fn xml_attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = attrs;
    while let Some(pos) = rest.find(name) {
        let before = rest[..pos].chars().next_back();
        if before.is_some_and(|ch| !ch.is_ascii_whitespace()) {
            rest = &rest[pos + name.len()..];
            continue;
        }
        let after_name = rest[pos + name.len()..].trim_start();
        let Some(after_eq) = after_name.strip_prefix('=') else {
            rest = &rest[pos + name.len()..];
            continue;
        };
        let after_eq = after_eq.trim_start();
        let quote = after_eq.chars().next()?;
        if !matches!(quote, '"' | '\'') {
            return None;
        }
        let value_start = &after_eq[quote.len_utf8()..];
        let end = value_start.find(quote)?;
        return Some(value_start[..end].trim());
    }
    None
}
