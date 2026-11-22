use super::super::parser::{Ecosystem, ManifestParser, ParsedManifest};

/// Parser for JVM build manifests (`pom.xml`, `build.gradle`, `build.gradle.kts`).
pub struct JvmParser;

/// Shared JVM manifest parser instance.
pub static JVM_PARSER: JvmParser = JvmParser;

impl ManifestParser for JvmParser {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Jvm
    }

    fn filenames(&self) -> &'static [&'static str] {
        &["pom.xml", "build.gradle", "build.gradle.kts"]
    }

    fn parse(&self, filename: &str, content: &str) -> ParsedManifest {
        let mut parsed = ParsedManifest::new(Ecosystem::Jvm);
        parsed.dependencies = crate::languages::build_manifest::dependencies(filename, content);
        parsed
    }
}
