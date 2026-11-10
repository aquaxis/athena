//! The `skill *` verbs (spec §24.3's command row for skills; X-32): the arg parse, the
//! request plan, and — riding [`crate::commands::execute_plan`] — the request they
//! perform against the resolved Core API.
//!
//! Two faces, one source of truth, exactly like the other §24.3 verbs (X-27's rule,
//! `R-IMPL-03`): the plan is the deterministic face the verb→endpoint tests assert and
//! `--dry-run` reports; everything else performs. The plans ride the §22.7 per-version
//! endpoints (`ATH-API-026`: the pin never floats):
//!
//! ```text
//! athena skill list                    — GET    /api/v1/skills
//! athena skill show <id>               — GET    /api/v1/skills/{id}
//! athena skill search <capability>     — GET    /api/v1/skills/search?capability=…
//! athena skill install <dir>           — POST   /api/v1/skills  ({"manifest_yaml": …})
//! athena skill enable <id> <version>   — POST   /api/v1/skills/{id}/versions/{v}/enable
//! athena skill disable <id> <version>  — POST   /api/v1/skills/{id}/versions/{v}/disable
//! athena skill update <id> <version>   — POST   /api/v1/skills/{id}/versions/{v}/update
//! athena skill validate <id> <version> — POST   /api/v1/skills/{id}/versions/{v}/validate
//! athena skill remove <id> <version>   — DELETE /api/v1/skills/{id}/versions/{v}
//! ```
//!
//! `install` is the §12.7 `register(skill)` operation under §24.3's command name — one
//! registry row per `(id, version)`, the manifest pins both; the equivalence is recorded
//! rather than a second operation. The mandatory `(id, version)` pin (`ATH-SKILL-020`,
//! `ATH-API-026`) is enforced at parse time: a missing pin is a usage fault (exit 2,
//! `ATH-CLI-008`).
//!
//! `--json` (`ATH-CLI-007`) carries the structured payload on both faces.

use crate::Outcome;
use crate::commands::{Plan, execute_plan, render_plan, strip_flags, usage};
use std::path::Path;

/// Run one `skill *` invocation (`args` begins after "skill").
///
/// The verbs perform their requests against the resolved Core API like every other §24.3
/// verb (`R-IMPL-03`); `--dry-run` renders the plan only.
#[must_use]
pub fn run_skill(args: &[String], cwd: &Path) -> Outcome {
    let json_wanted = args.iter().any(|arg| arg == "--json");
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let words = strip_flags(args);
    let plan = match skill_plan(&words, cwd) {
        Ok(plan) => plan,
        Err(outcome) => return outcome,
    };
    if dry_run {
        return render_plan(&plan, json_wanted);
    }
    let Some(home) = crate::resolve::resolve_home(cwd) else {
        // The address lives in the project's configuration, so without a project there is
        // nothing to talk to (`ATH-CLI-010`).
        return Outcome::fail(
            2,
            format!(
                "no .athena/ resolved from {}; run `athena init` to introduce this \
                 project first (ATH-CLI-010)",
                cwd.display()
            ),
        );
    };
    let endpoint = match crate::endpoint::resolve(&home, token_override(&words).as_deref()) {
        Ok(endpoint) => endpoint,
        Err(fault) => return Outcome::fail(2, format!("athena skill: {fault}")),
    };
    execute_plan(&endpoint, &plan, json_wanted)
}

/// The `--token <value>` override, when present (the flag wins, as everywhere).
fn token_override(args: &[String]) -> Option<String> {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if word == "--token" {
            return words.next().cloned();
        }
    }
    None
}

/// Parse one `skill *` invocation into the request it makes (`args` begins after
/// "skill", flags stripped).
fn skill_plan(args: &[String], cwd: &Path) -> Result<Plan, Outcome> {
    match args.first().map(String::as_str) {
        Some("list") => Ok(Plan {
            method: "GET",
            path: "/api/v1/skills".into(),
            body_json: None,
        }),
        Some("show") => match args.get(1) {
            Some(id) if !id.trim().is_empty() => Ok(Plan {
                method: "GET",
                path: format!("/api/v1/skills/{id}"),
                body_json: None,
            }),
            _ => Err(usage("usage: athena skill show <id>")),
        },
        Some("search") => match args.get(1) {
            Some(capability) if !capability.trim().is_empty() => Ok(Plan {
                method: "GET",
                // The capability rides the query percent-encoded (RFC 3986 unreserved set
                // stays raw; everything else — including the space and the multibyte
                // Japanese capability wording — becomes `%XX`); the server decodes the
                // value once before the Catalog's exact-text comparison (`ATH-SKILL-006`).
                path: format!(
                    "/api/v1/skills/search?capability={}",
                    encode_query_value(capability)
                ),
                body_json: None,
            }),
            _ => Err(usage("usage: athena skill search <capability>")),
        },
        Some("install" | "register") => match args.get(1) {
            Some(directory) if !directory.trim().is_empty() => install_plan(cwd, directory),
            _ => Err(usage("usage: athena skill install <directory>")),
        },
        Some(verb @ ("enable" | "disable" | "update" | "validate")) => {
            pin_verb_plan(verb, &args[1..])
        }
        Some("remove") => pin_verb_plan("remove", &args[1..]),
        _ => Err(usage(
            "usage: athena skill <install|list|show|search|enable|disable|update|validate|remove> …",
        )),
    }
}

/// `install <directory>`: read the directory's `manifest.yaml` and carry it as the
/// registration body the API converts (`{"manifest_yaml": …}`). The manifest pins
/// `(id, version)` itself (`ATH-SKILL-020`; `ATH-SKILL-003`).
fn install_plan(cwd: &Path, directory: &str) -> Result<Plan, Outcome> {
    let path = cwd.join(directory.trim()).join("manifest.yaml");
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::to_string(&serde_json::json!({ "manifest_yaml": text })) {
            Ok(body) => Ok(Plan {
                method: "POST",
                path: "/api/v1/skills".into(),
                body_json: Some(body),
            }),
            Err(detail) => Err(Outcome::fail(4, format!("athena skill install: {detail}"))),
        },
        Err(error) => Err(Outcome::fail(
            4,
            format!(
                "athena skill install: cannot read {}: {error}",
                path.display()
            ),
        )),
    }
}

/// The per-version face one pinned `(id, version)` rides (§22.7's form; the pin is
/// mandatory — a missing half is a usage fault, exit 2).
fn pin_verb_plan(verb: &str, args: &[String]) -> Result<Plan, Outcome> {
    let (Some(id), Some(version)) = (args.first(), args.get(1)) else {
        return Err(usage(&format!(
            "usage: athena skill {verb} <id> <version> (the pin is mandatory, ATH-SKILL-020)"
        )));
    };
    if id.trim().is_empty() || version.trim().is_empty() {
        return Err(usage(&format!(
            "usage: athena skill {verb} <id> <version> (the pin is mandatory, ATH-SKILL-020)"
        )));
    }
    if verb == "remove" {
        Ok(Plan {
            method: "DELETE",
            path: format!("/api/v1/skills/{id}/versions/{version}"),
            body_json: None,
        })
    } else {
        Ok(Plan {
            method: "POST",
            path: format!("/api/v1/skills/{id}/versions/{version}/{verb}"),
            body_json: None,
        })
    }
}

/// Encode one query value per RFC 3986 §2: the unreserved set stays raw, everything
/// else — the space and the multibyte text alike — rides `%XX` (uppercase hex). The
/// server's search face decodes the value once before the Catalog's exact-text
/// comparison (`ATH-SKILL-006`), so the two sides are symmetric.
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(*byte));
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn the_search_plan_encodes_the_capability_query_value() {
        // The multibyte capability wording rides encoded (`ATH-SKILL-006`'s exact text
        // is preserved across the wire; the server decodes once):
        let plan = skill_plan(
            &words(&["search", "引き渡しゲート判定"]),
            Path::new("/tmp/p"),
        )
        .expect("a well-formed search");
        assert_eq!(plan.method, "GET");
        assert_eq!(
            plan.path,
            "/api/v1/skills/search?capability=%E5%BC%95%E3%81%8D%E6%B8%A1%E3%81%97%E3%82%B2%E3%83%BC%E3%83%88%E5%88%A4%E5%AE%9A"
        );
        // The raw space becomes `%20` (not the form cross `+`); the unreserved set
        // stays raw:
        let plan = skill_plan(&words(&["search", "rust lint"]), Path::new("/tmp/p"))
            .expect("a well-formed search");
        assert_eq!(plan.path, "/api/v1/skills/search?capability=rust%20lint");
        let plan = skill_plan(&words(&["search", "fpga"]), Path::new("/tmp/p"))
            .expect("a well-formed search");
        assert_eq!(plan.path, "/api/v1/skills/search?capability=fpga");
    }

    #[test]
    fn a_search_without_a_capability_is_a_usage_fault() {
        for shape in [words(&["search"]), words(&["search", "   "])] {
            let outcome = skill_plan(&shape, Path::new("/tmp/p")).expect_err("a usage fault");
            assert_eq!(outcome.code, 2, "{shape:?} (ATH-CLI-008)");
        }
    }

    #[test]
    fn the_encode_is_the_server_decoder_s_inverse() {
        for value in [
            "fpga",
            "rust lint",
            "引き渡しゲート判定",
            "a+b",
            "100%",
            "tilde~",
        ] {
            let encoded = encode_query_value(value);
            // The raw `%XX` runs ride verbatim through the server-side decode face —
            // byte-wise, so multibyte UTF-8 reassembles only at the end:
            assert!(!encoded.contains(' '), "{value:?}");
            let mut decoded: Vec<u8> = Vec::with_capacity(encoded.len());
            let bytes = encoded.as_bytes();
            let mut at = 0;
            while at < bytes.len() {
                let hex = (bytes[at] == b'%')
                    .then(|| bytes.get(at + 1..at + 3))
                    .flatten()
                    .and_then(|slice| std::str::from_utf8(slice).ok())
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                if let Some(code) = hex {
                    decoded.push(code);
                    at += 3;
                } else {
                    decoded.push(bytes[at]);
                    at += 1;
                }
            }
            let round = String::from_utf8(decoded).expect("utf-8");
            assert_eq!(round, value, "the decode round-trips {value:?}");
        }
    }
}
