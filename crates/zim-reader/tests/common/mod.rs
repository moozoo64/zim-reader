use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub fn fixture_path(rel: &str) -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest_dir.join("tests/fixtures").join(rel);
    candidate.exists().then_some(candidate)
}

#[allow(dead_code)]
pub fn with_fixture(rel: &str, body: impl FnOnce(PathBuf)) {
    match fixture_path(rel) {
        Some(p) => body(p),
        None => eprintln!(
            "SKIP: fixture {rel} not found (run `git submodule update --init --recursive`)"
        ),
    }
}
