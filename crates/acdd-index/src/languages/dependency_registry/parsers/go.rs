use super::super::parser::{Ecosystem, ManifestParser, ModulePrefix, ParsedManifest};

/// Parser for `go.mod` manifests.
pub(crate) struct GoParser;

/// Shared `go.mod` manifest parser instance.
pub(crate) static GO_PARSER: GoParser = GoParser;

impl ManifestParser for GoParser {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Go
    }

    fn filenames(&self) -> &'static [&'static str] {
        &["go.mod"]
    }

    fn parse(&self, _filename: &str, content: &str) -> ParsedManifest {
        let mut parsed = ParsedManifest::new(Ecosystem::Go);
        let mut in_require_block = false;
        for raw in content.lines() {
            let line = raw.split_once("//").map_or(raw, |(before, _)| before).trim();
            if line.is_empty() {
                continue;
            }
            if in_require_block {
                if line.starts_with(')') {
                    in_require_block = false;
                } else if let Some(module) = first_module_token(line) {
                    parsed.dependencies.push(module);
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("module") {
                if rest.chars().next().is_some_and(char::is_whitespace) {
                    if let Some(module_path) = first_module_token(rest) {
                        if !parsed
                            .module_prefixes
                            .iter()
                            .any(|existing| existing.prefix == module_path)
                        {
                            parsed.own_packages.push(module_path.clone());
                            parsed.module_prefixes.push(ModulePrefix {
                                ecosystem: Ecosystem::Go,
                                prefix: module_path,
                                source_root: String::new(),
                                origin: String::new(),
                            });
                        }
                    }
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("require") {
                if !rest
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_whitespace() || ch == '(')
                {
                    continue;
                }
                let rest = rest.trim();
                if let Some(after_paren) = rest.strip_prefix('(') {
                    if let Some((inline, _)) = after_paren.split_once(')') {
                        if let Some(module) = first_module_token(inline) {
                            parsed.dependencies.push(module);
                        }
                    } else {
                        if let Some(module) = first_module_token(after_paren) {
                            parsed.dependencies.push(module);
                        }
                        in_require_block = true;
                    }
                } else if let Some(module) = first_module_token(rest) {
                    parsed.dependencies.push(module);
                }
            }
        }
        parsed.finish()
    }
}

fn first_module_token(line: &str) -> Option<String> {
    let token = line.split_whitespace().next()?;
    let token = token.trim_matches(['"', '`']);
    (!token.is_empty()).then(|| token.to_owned())
}
