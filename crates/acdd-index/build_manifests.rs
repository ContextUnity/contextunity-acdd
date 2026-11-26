use std::path::{Path, PathBuf};

fn collect_framework_manifests(directory: &Path, manifests: &mut Vec<(String, String, PathBuf)>) {
    println!("cargo:rerun-if-changed={}", directory.display());
    let mut entries = std::fs::read_dir(directory)
        .expect("read framework manifest directory")
        .map(|entry| entry.expect("read framework manifest entry"))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().expect("read framework manifest type");
        assert!(
            !file_type.is_symlink(),
            "framework manifest must not be a symlink"
        );
        if file_type.is_dir() {
            collect_framework_manifests(&path, manifests);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(extension @ ("toml" | "yaml" | "yml")) =
            path.extension().and_then(|value| value.to_str())
        else {
            continue;
        };
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .expect("UTF-8 framework manifest identity");
        manifests.push((name.to_owned(), extension.to_owned(), path));
    }
}

pub fn generate_framework_manifests(directory_path: &str) -> Vec<PathBuf> {
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    generate_framework_manifests_to(Path::new(directory_path), &output)
}

pub fn generate_framework_manifests_to(directory: &Path, out_dir: &Path) -> Vec<PathBuf> {
    let root_meta =
        std::fs::symlink_metadata(directory).expect("read framework manifest root metadata");
    assert!(
        !root_meta.file_type().is_symlink(),
        "framework manifest root must not be a symlink"
    );
    assert!(
        root_meta.file_type().is_dir(),
        "framework manifest root must be a directory"
    );
    let mut manifests = Vec::new();
    collect_framework_manifests(directory, &mut manifests);
    manifests.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.2.cmp(&right.2)));
    for pair in manifests.windows(2) {
        assert_ne!(
            pair[0].0, pair[1].0,
            "duplicate framework manifest identity"
        );
    }
    let mut source = String::from("&[\n");
    for (name, extension, path) in &manifests {
        let absolute = path
            .canonicalize()
            .expect("canonical framework manifest path");
        source.push_str(&format!(
            "({name:?}, {extension:?}, include_str!({:?})),\n",
            absolute.to_str().expect("UTF-8 framework manifest path")
        ));
    }
    source.push_str("]\n");
    std::fs::write(out_dir.join("framework_manifests.rs"), source)
        .expect("write framework inventory");
    manifests.into_iter().map(|(_, _, path)| path).collect()
}
