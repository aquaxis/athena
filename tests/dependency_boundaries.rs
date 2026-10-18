//! Module boundary enforcement (spec section 32.2, ATH-DEV-004, ATH-DEV-011, ATH-DEV-023).
//!
//! Three boundaries are normative; crate names are merely their expression:
//!   1. Core does not depend on a specific Runtime (ATH-RT-007).
//!   2. UI reaches Core only through the Core API (ATH-API-001).
//!   3. Libraries do not depend on binaries.
//!
//! Any workspace-internal edge absent from `ALLOWED` fails the test: an unlisted dependency is
//! not implicitly permitted and must be added to the specification first (ATH-DEV-023).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Permitted workspace-internal dependency edges, from spec section 32.2.
const ALLOWED: &[(&str, &str)] = &[
    ("athena-api", "athena-core"),
    ("athena-api", "athena-types"),
    // The resident process is the composition root (spec §32.2, revised 2026-10-18):
    // it links the real Adapter and registers it into the registry at boot. Core itself
    // still never references an Adapter (athena-core -> runtimes/* stays forbidden).
    ("athena-api", "athena-runtime-agent-cli"),
    ("athena-core", "athena-agent"),
    ("athena-core", "athena-messaging"),
    ("athena-core", "athena-memory"),
    ("athena-core", "athena-tools"),
    ("athena-core", "athena-skills"),
    ("athena-core", "athena-state"),
    ("athena-core", "athena-events"),
    ("athena-core", "athena-persistence"),
    ("athena-core", "athena-config"),
    ("athena-core", "athena-runtime"),
    ("athena-core", "athena-types"),
    ("athena-runtime", "athena-types"),
    ("athena-skills", "athena-types"),
    // runtimes/* -> athena-runtime, athena-skills, athena-types.
    // Permitted because Skill Manager belongs to the Runtime abstraction layer (ATH-SKILL-012)
    // and the Adapter places resolved skills into the Instance area (ATH-SKILL-017).
    ("athena-runtime-agent-cli", "athena-runtime"),
    ("athena-runtime-agent-cli", "athena-skills"),
    ("athena-runtime-agent-cli", "athena-types"),
    // Remaining library crates depend only on shared types.
    ("athena-agent", "athena-types"),
    ("athena-config", "athena-types"),
    ("athena-events", "athena-types"),
    ("athena-memory", "athena-types"),
    ("athena-messaging", "athena-types"),
    ("athena-persistence", "athena-types"),
    ("athena-state", "athena-types"),
    ("athena-tools", "athena-types"),
];

/// Edges that must never exist, kept explicit so a violation names the breached rule.
const FORBIDDEN: &[(&str, &str, &str)] = &[
    (
        "athena-core",
        "athena-runtime-agent-cli",
        "Core must not depend on a Runtime Adapter (ATH-RT-007, ATH-DEV-012)",
    ),
    (
        "athena-studio",
        "athena-core",
        "UI must reach Core only through the Core API (ATH-POLICY-007, ATH-API-001)",
    ),
    (
        "athena-cli",
        "athena-core",
        "UI must reach Core only through the Core API (ATH-POLICY-007, ATH-API-001)",
    ),
    (
        "athena-runtime-agent-cli",
        "athena-core",
        "an Adapter must not depend on Core Host",
    ),
    (
        "athena-runtime",
        "athena-runtime-agent-cli",
        "the Runtime Interface must not reference an Adapter (ATH-DEV-012)",
    ),
];

/// Binary crates: nothing in the workspace may depend on them.
const BINARIES: &[&str] = &["athena-api", "athena-cli", "athena-studio"];

/// Walk upward from this package until the manifest declaring `[workspace]` is found.
///
/// `CARGO_MANIFEST_DIR` points at this package (`tests/`), not the workspace root, and the
/// search keeps the check correct if the package is ever relocated.
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

/// Collect `(crate, dependency)` edges by reading each member manifest.
///
/// Parsing is deliberately textual rather than via `cargo metadata`: the check must hold even
/// when the workspace does not build, and the declared edges in each `Cargo.toml` are exactly
/// what spec section 32.2 governs.
fn edges() -> Vec<(String, String)> {
    let root = workspace_root();
    let mut members: Vec<PathBuf> = Vec::new();
    for dir in ["crates", "runtimes"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let manifest = entry.path().join("Cargo.toml");
            if manifest.is_file() {
                members.push(manifest);
            }
        }
    }
    assert!(
        !members.is_empty(),
        "no workspace members found under crates/ or runtimes/"
    );

    let mut found = Vec::new();
    for manifest in members {
        let text = std::fs::read_to_string(&manifest).expect("read manifest");
        let name = text
            .lines()
            .find_map(|line| {
                line.strip_prefix("name = \"")
                    .and_then(|rest| rest.strip_suffix('"'))
            })
            .unwrap_or_else(|| panic!("no package name in {}", manifest.display()))
            .to_string();

        // Workspace-internal dependencies are declared as `athena-x = { workspace = true }`
        // or with an explicit `path = `. Either form counts as an edge.
        let mut in_deps = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_deps = trimmed.starts_with("[dependencies")
                    || trimmed.starts_with("[dev-dependencies")
                    || trimmed.starts_with("[build-dependencies");
                continue;
            }
            if !in_deps {
                continue;
            }
            let Some(dep) = trimmed.split('=').next().map(str::trim) else {
                continue;
            };
            if dep.starts_with("athena-") {
                found.push((name.clone(), dep.to_string()));
            }
        }
    }
    found
}

#[test]
fn no_forbidden_dependency_exists() {
    let edges = edges();
    for (from, to, rule) in FORBIDDEN {
        let violated = edges.iter().any(|(f, t)| f == from && t == to);
        assert!(!violated, "forbidden dependency {from} -> {to}: {rule}");
    }
}

#[test]
fn every_edge_is_explicitly_allowed() {
    let allowed: BTreeSet<(&str, &str)> = ALLOWED.iter().copied().collect();
    let mut unlisted = Vec::new();
    for (from, to) in edges() {
        if !allowed.contains(&(from.as_str(), to.as_str())) {
            unlisted.push(format!("{from} -> {to}"));
        }
    }
    assert!(
        unlisted.is_empty(),
        "dependency edges absent from the spec section 32.2 allow-list (ATH-DEV-023 requires \
         adding them to the specification as permitted or forbidden before use): {unlisted:#?}"
    );
}

#[test]
fn no_crate_depends_on_a_binary() {
    for (from, to) in edges() {
        assert!(
            !BINARIES.contains(&to.as_str()),
            "{from} depends on binary crate {to}; libraries must not depend on binaries"
        );
    }
}

#[test]
fn allow_list_itself_stays_consistent_with_the_forbidden_list() {
    // A pair must not appear in both lists, or the enforcement would be self-contradictory.
    let allowed: BTreeSet<(&str, &str)> = ALLOWED.iter().copied().collect();
    for (from, to, _) in FORBIDDEN {
        assert!(
            !allowed.contains(&(from, to)),
            "{from} -> {to} is listed as both allowed and forbidden"
        );
    }
}
