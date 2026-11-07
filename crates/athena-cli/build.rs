//! Embed the repository's distributed default definitions for `athena init`.
//!
//! `athena init` copies the repository's `personas/` and `skills/` into the target
//! project (spec section 32.1). Enumerating those files by hand in `template.rs` made
//! every added definition a code change that could be forgotten, and it left the
//! project's first `athena init` without the definitions a later `athena update` would
//! have added. This build script walks the two directories and generates a `&[(&str,
//! &str)]` of `(relative path, content)` pairs with `include_str!`, so the binary
//! carries exactly what the repository holds at build time.
//!
//! `cargo:rerun-if-changed` is emitted for both directories, so adding, removing or
//! editing a definition rebuilds this crate.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/athena-cli sits two levels under the repository root")
        .to_path_buf();

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for dir in ["personas", "skills"] {
        let root = repo_root.join(dir);
        if !root.is_dir() {
            continue;
        }
        collect(&root, &root, dir, &mut entries);
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // The generated file is `include!`d inside a `pub const ... = ` declaration in
    // `template.rs`, so it must be a bare array expression and nothing else.
    let mut generated = String::new();
    generated.push_str("&[\n");
    for (rel_key, abs_path) in &entries {
        // The literal path must be escaped for a Rust string; repository paths are
        // plain ASCII here, but escape defensively so a stray character cannot
        // produce invalid source.
        let escaped = abs_path.display().to_string().replace('\\', "\\\\");
        let _ = writeln!(
            generated,
            "    (\"{rel_key}\", include_str!(\"{escaped}\")),"
        );
    }
    generated.push_str("]\n");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("out dir"));
    std::fs::write(out_dir.join("embedded_definitions.rs"), generated)
        .expect("write the generated definitions");

    for dir in ["personas", "skills"] {
        println!("cargo:rerun-if-changed={}", repo_root.join(dir).display());
    }
}

/// Collect every regular file beneath `root`, keyed as `<prefix>/<relative path>` with
/// forward slashes, recursing into subdirectories. Hidden entries (a dot-leading name)
/// are skipped: they are never shipped definitions, and a stray editor swap file must
/// not enter the binary.
fn collect(root: &Path, dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, prefix, out);
        } else if path.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push((format!("{prefix}/{rel}"), path));
        }
    }
}
