use super::super::parser::{Ecosystem, ManifestParser, ModulePrefix, ParsedManifest};

/// Parser for PHP `composer.json` manifests (dependencies and PSR-4 autoload prefixes).
pub(crate) struct PhpParser;

/// Shared PHP manifest parser instance.
pub(crate) static PHP_PARSER: PhpParser = PhpParser;

impl ManifestParser for PhpParser {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Php
    }

    fn filenames(&self) -> &'static [&'static str] {
        &["composer.json"]
    }

    fn parse(&self, _filename: &str, content: &str) -> ParsedManifest {
        let mut parsed = ParsedManifest::new(Ecosystem::Php);
        let Ok(root) = serde_json::from_str::<serde_json::Value>(content) else {
            return parsed;
        };
        if let Some(name) = root
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.trim().is_empty())
        {
            parsed.own_packages.push(name.to_owned());
        }
        for section in ["require", "require-dev"] {
            if let Some(entries) = root.get(section).and_then(serde_json::Value::as_object) {
                for package in entries.keys() {
                    if !is_composer_platform_package(package) {
                        parsed.dependencies.push(package.clone());
                    }
                }
            }
        }
        for section in ["autoload", "autoload-dev"] {
            let Some(psr4) = root
                .get(section)
                .and_then(|value| value.get("psr-4"))
                .and_then(serde_json::Value::as_object)
            else {
                continue;
            };
            for (namespace, targets) in psr4 {
                let prefix = namespace.trim().trim_end_matches('\\');
                if prefix.is_empty() {
                    continue;
                }
                let dirs = targets.as_str().into_iter().chain(
                    targets
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(serde_json::Value::as_str),
                );
                for raw_dir in dirs {
                    if let Some(source_root) = normalize_psr4_dir(raw_dir) {
                        parsed.module_prefixes.push(ModulePrefix {
                            ecosystem: Ecosystem::Php,
                            prefix: prefix.to_owned(),
                            source_root,
                            origin: String::new(),
                        });
                    }
                }
            }
        }
        parsed.finish()
    }
}

fn is_composer_platform_package(name: &str) -> bool {
    matches!(
        name,
        "php" | "php-64bit" | "composer-plugin-api" | "composer-runtime-api"
    ) || name.starts_with("ext-")
        || name.starts_with("lib-")
}

fn normalize_psr4_dir(raw: &str) -> Option<String> {
    let trimmed = raw.trim().replace('\\', "/");
    if trimmed.starts_with('/') {
        return None;
    }
    let cleaned = trimmed.trim_start_matches("./").trim_matches('/');
    if cleaned.is_empty() {
        return Some(String::new());
    }
    if cleaned
        .split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return None;
    }
    Some(cleaned.to_owned())
}
