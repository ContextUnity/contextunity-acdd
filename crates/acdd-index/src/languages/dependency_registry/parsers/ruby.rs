use super::super::parser::{Ecosystem, ManifestParser, ParsedManifest};

/// Parser for Ruby manifests (`Gemfile`, `*.gemspec`).
pub struct RubyParser;

/// Shared Ruby manifest parser instance.
pub static RUBY_PARSER: RubyParser = RubyParser;

impl ManifestParser for RubyParser {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Ruby
    }

    fn filenames(&self) -> &'static [&'static str] {
        &["Gemfile", "*.gemspec"]
    }

    fn parse(&self, filename: &str, content: &str) -> ParsedManifest {
        let mut parsed = ParsedManifest::new(Ecosystem::Ruby);
        let is_gemspec = filename.ends_with(".gemspec");
        for raw in content.lines() {
            let line = raw.split('#').next().unwrap_or(raw).trim();
            if line.is_empty() {
                continue;
            }
            let gem_arg = if is_gemspec {
                [
                    "add_dependency",
                    "add_runtime_dependency",
                    "add_development_dependency",
                ]
                .iter()
                .find_map(|method| {
                    line.find(method).and_then(|pos| {
                        let before = line[..pos].chars().next_back();
                        let valid_before =
                            before.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
                        valid_before.then(|| &line[pos + method.len()..])
                    })
                })
            } else {
                line.strip_prefix("gem")
            };
            let Some(rest) = gem_arg else {
                continue;
            };
            if !rest
                .chars()
                .next()
                .is_some_and(|ch| ch.is_whitespace() || ch == '(')
            {
                continue;
            }
            if let Some(name) = extract_quoted_ruby_string(rest.trim_start_matches([' ', '\t', '('])) {
                parsed.dependencies.push(name.to_owned());
            }
        }
        parsed.finish()
    }
}

fn extract_quoted_ruby_string(text: &str) -> Option<&str> {
    let quote = text.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let body = &text[quote.len_utf8()..];
    let end = body.find(quote)?;
    let value = body[..end].trim();
    (!value.is_empty()).then_some(value)
}
