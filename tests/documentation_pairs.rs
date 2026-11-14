//! Conformance test for the documentation pairs' own stated invariant (X-64).
//!
//! Every document under `docs/` declares, in its own header table, that its counterpart
//! carries 同一内容 — the same content in the other language. Nothing enforced it, and two
//! pairs had drifted apart by the time a survey measured them: the English
//! `should_compliance` recorded an NFR row as unmeasured while the Japanese side had been
//! corrected, and the English `license-provenance` was missing a section its Japanese
//! counterpart carried. Both were prose defects that review had passed over twice.
//!
//! Content equality across languages is not mechanically checkable, but *structure* is, and
//! structure is what drifts when one side gains a row or a section the other never gets.
//! This test holds the two counts the drifts showed up in — table rows and headings — plus
//! the existence of the pair itself.

use std::path::{Path, PathBuf};

/// Walk upward until the manifest declaring `[workspace]` is found (the sibling
/// `dependency_boundaries` walk — `CARGO_MANIFEST_DIR` points at this package).
fn workspace_root() -> PathBuf {
    let mut dir: &Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let manifest = dir.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest)
            && text.contains("[workspace]")
        {
            return dir.to_path_buf();
        }
        dir = dir
            .parent()
            .expect("walked past the filesystem root without finding a [workspace] manifest");
    }
}

/// `(table rows, headings)` — the two shapes a one-sided edit moves.
fn shape(text: &str) -> (usize, usize) {
    let rows = text.lines().filter(|line| line.starts_with('|')).count();
    let headings = text.lines().filter(|line| line.starts_with('#')).count();
    (rows, headings)
}

#[test]
fn every_english_document_has_a_japanese_counterpart_of_the_same_shape() {
    let docs = workspace_root().join("docs");
    let mut checked = 0;

    for entry in std::fs::read_dir(&docs).expect("docs/ is readable") {
        let path = entry.expect("a readable entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // The English side is every `*.md` that is not itself a `_ja.md`.
        if !name.ends_with(".md") || name.ends_with("_ja.md") {
            continue;
        }
        let stem = name.trim_end_matches(".md");
        let counterpart = docs.join(format!("{stem}_ja.md"));
        assert!(
            counterpart.is_file(),
            "docs/{name} declares a Japanese counterpart; docs/{stem}_ja.md is absent"
        );

        let english = std::fs::read_to_string(&path).expect("readable");
        let japanese = std::fs::read_to_string(&counterpart).expect("readable");
        let (en_rows, en_heads) = shape(&english);
        let (ja_rows, ja_heads) = shape(&japanese);

        assert_eq!(
            en_rows, ja_rows,
            "docs/{stem}: the pair's table-row counts differ ({en_rows} vs {ja_rows}); one \
             side gained or lost a row the other did not — the pair claims 同一内容"
        );
        assert_eq!(
            en_heads, ja_heads,
            "docs/{stem}: the pair's heading counts differ ({en_heads} vs {ja_heads}); one \
             side gained or lost a section the other did not — the pair claims 同一内容"
        );
        checked += 1;
    }

    assert!(
        checked >= 8,
        "the documentation set is eight pairs or more; only {checked} were checked, so the \
         walk is not seeing docs/"
    );
}
