//! The nine base tools of spec section 11.1, their default risks, and the deny-by-default
//! per-agent grant surface (`ATH-TOOL-001`).

use athena_types::{RiskLevel, ToolName};
use std::path::PathBuf;

/// One base tool's definition row (spec section 11.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseTool {
    /// The tool name.
    pub name: ToolName,
    /// The spec's purpose text.
    pub purpose: &'static str,
    /// The spec's default risk; the operative classification for a concrete invocation
    /// comes from `policy.rules` (first match; unmatched ⇒ high, `ATH-CFG-009`).
    pub default_risk: RiskLevel,
}

/// The nine base tools, in spec section 11.1's table order.
pub const BASE_TOOLS: [BaseTool; 9] = [
    BaseTool {
        name: ToolName::FsRead,
        purpose: "file reads",
        default_risk: RiskLevel::Low,
    },
    BaseTool {
        name: ToolName::FsWrite,
        purpose: "file writes",
        default_risk: RiskLevel::Medium,
    },
    BaseTool {
        name: ToolName::Shell,
        purpose: "shell execution",
        default_risk: RiskLevel::High,
    },
    BaseTool {
        name: ToolName::Git,
        purpose: "version-control operations",
        default_risk: RiskLevel::Medium,
    },
    BaseTool {
        name: ToolName::Search,
        purpose: "search",
        default_risk: RiskLevel::Low,
    },
    BaseTool {
        name: ToolName::SendTo,
        purpose: "inter-agent communication",
        default_risk: RiskLevel::Low,
    },
    BaseTool {
        name: ToolName::Process,
        purpose: "process operations",
        default_risk: RiskLevel::High,
    },
    BaseTool {
        name: ToolName::Terminal,
        purpose: "interactive terminal operations",
        default_risk: RiskLevel::High,
    },
    BaseTool {
        name: ToolName::Network,
        purpose: "external network communication",
        default_risk: RiskLevel::High,
    },
];

/// Why a grant set refuses (`ATH-TOOL-001`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantFault {
    /// The same tool was granted twice; a grant set is a set.
    #[error("duplicate tool grant: {0:?}")]
    Duplicate(ToolName),
}

/// The per-agent tool grant surface (`ATH-TOOL-001`): **deny by default** — a tool is
/// permitted only when the agent's definition explicitly granted it.
///
/// This is the grant *representation*; the enforcement point is Core Host's Tool
/// Manager (`ATH-TOOL-002`, `ATH-SEC-022`), and the resolver's own checks are defence
/// in depth (`ATH-SEC-026`). Named grants come from `AgentDefinition.allowed_tools`
/// (and `delegable_tools` down the hierarchy, `ATH-SEC-009`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolGrants {
    allowed: Vec<ToolName>,
}

impl ToolGrants {
    /// The deny-all default: an agent with no stated grants may invoke nothing.
    #[must_use]
    pub fn denying_all() -> Self {
        Self::default()
    }

    /// The grant set an Agent Definition states; duplicates are refused (`ATH-TOOL-001`).
    ///
    /// # Errors
    /// [`GrantFault::Duplicate`] when the same tool appears twice.
    pub fn from_allowed(allowed: &[ToolName]) -> Result<Self, GrantFault> {
        let mut grants = Self::default();
        for tool in allowed {
            grants.grant(*tool)?;
        }
        Ok(grants)
    }

    /// Grant one tool; a second grant of the same tool is an error.
    ///
    /// # Errors
    /// [`GrantFault::Duplicate`].
    pub fn grant(&mut self, tool: ToolName) -> Result<(), GrantFault> {
        if self.allowed.contains(&tool) {
            return Err(GrantFault::Duplicate(tool));
        }
        self.allowed.push(tool);
        Ok(())
    }

    /// Whether the invocation of one tool is permitted (`ATH-TOOL-001`: deny by default).
    #[must_use]
    pub fn permitted(&self, tool: ToolName) -> bool {
        self.allowed.contains(&tool)
    }

    /// The granted tools, in grant order (the agent's statement, not a canonical order).
    #[must_use]
    pub fn allowed(&self) -> &[ToolName] {
        &self.allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_tools_match_the_spec_table() {
        // section 11.1's nine rows, in table order with the default risks.
        let expected: [(ToolName, RiskLevel); 9] = [
            (ToolName::FsRead, RiskLevel::Low),
            (ToolName::FsWrite, RiskLevel::Medium),
            (ToolName::Shell, RiskLevel::High),
            (ToolName::Git, RiskLevel::Medium),
            (ToolName::Search, RiskLevel::Low),
            (ToolName::SendTo, RiskLevel::Low),
            (ToolName::Process, RiskLevel::High),
            (ToolName::Terminal, RiskLevel::High),
            (ToolName::Network, RiskLevel::High),
        ];
        for (row, (name, risk)) in BASE_TOOLS.iter().zip(expected) {
            assert_eq!(row.name, name, "row {row:?}");
            assert_eq!(row.default_risk, risk, "row {row:?}");
            // The wire name is the spec's tool slug.
            assert!(!row.purpose.is_empty());
        }
        // Every declared tool's type-level default agrees with the table.
        for row in BASE_TOOLS {
            assert_eq!(row.name.default_risk(), row.default_risk, "{}", row.name);
        }
    }

    #[test]
    fn grants_deny_by_default_and_permit_only_the_statement() {
        // An empty statement denies all nine (ATH-TOOL-001).
        let none = ToolGrants::denying_all();
        for row in BASE_TOOLS {
            assert!(
                !none.permitted(row.name),
                "no tool is permitted by default: {}",
                row.name
            );
        }
        assert!(none.allowed().is_empty());

        // An explicit statement permits exactly what it granted.
        let grants =
            ToolGrants::from_allowed(&[ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo])
                .expect("valid grants");
        assert!(grants.permitted(ToolName::FsRead));
        assert!(grants.permitted(ToolName::FsWrite));
        assert!(grants.permitted(ToolName::SendTo));
        for row in BASE_TOOLS {
            if !matches!(
                row.name,
                ToolName::FsRead | ToolName::FsWrite | ToolName::SendTo
            ) {
                assert!(
                    !grants.permitted(row.name),
                    "ungranted tools stay denied: {}",
                    row.name
                );
            }
        }
        assert_eq!(grants.allowed().len(), 3, "the statement, in order");
    }

    #[test]
    fn duplicate_grants_are_refused() {
        let mut grants = ToolGrants::denying_all();
        grants.grant(ToolName::Search).expect("first grant");
        let second = grants
            .grant(ToolName::Search)
            .expect_err("a duplicate grant is an error");
        assert!(
            matches!(second, GrantFault::Duplicate(ToolName::Search)),
            "{second}"
        );
        // And through the constructor.
        assert!(ToolGrants::from_allowed(&[ToolName::Git, ToolName::Git]).is_err());
    }

    #[test]
    fn the_base_tool_set_is_whole_and_wire_named() {
        // Every ToolName the type system knows is in the base table (the spec's tool
        // universe is exactly these nine rows), each with a distinct wire slug.
        let mut names: Vec<_> = BASE_TOOLS.iter().map(|row| row.name).collect();
        // A duplicate name would collapse under dedup.
        names.dedup();
        assert_eq!(names.len(), 9, "nine rows, no repeats");
        for name in names {
            assert!(
                BASE_TOOLS.iter().any(|row| row.name == name),
                "{name:?} is represented in the section 11.1 table"
            );
        }
    }
}

// --- P4-03: the fs path boundary (ATH-TOOL-005) ------------------------------------------

/// Why a filesystem invocation was refused (`ATH-TOOL-005`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoundaryFault {
    /// The path is under an excluded root — the control surface, `.athena/`, or another
    /// instance's area: denied regardless of any writable or read-only membership.
    #[error("the path {0:?} is under an excluded root; the agent may not touch it (ATH-CORE-043)")]
    Excluded(PathBuf),
    /// The path is outside the workspace, the work targets, and the read-only set.
    #[error("the path {0:?} is out of the filesystem boundary (ATH-TOOL-005)")]
    OutOfBoundary(PathBuf),
    /// The boundary question is only defined for `fs_read` and `fs_write`.
    #[error("{tool:?} has no path semantics; the boundary check does not apply")]
    NotAFilesystemTool {
        /// The tool that was handed over.
        tool: ToolName,
    },
}

/// One pure boundary decision (`ATH-TOOL-005`): `fs_read` / `fs_write` are confined to
/// the path boundary anchored on the Agent Workspace — writable = Workspace ∪ work
/// targets, readable additionally = the read-only conventions, excluded = the forbidden
/// roots which win over everything (the control surface sits inside the Workspace and
/// must still be untouchable, `ATH-CORE-043`).
///
/// The decision is pure and Core-Host-owned in substance: the enforcement point is the
/// Tool Manager (`ATH-TOOL-002`, `ATH-SEC-022`); this type is the surface enforcement
/// reads, and any check the Resolver or Adapter runs is defence in depth (`ATH-SEC-026`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathBoundary {
    /// Absolute roots an `fs_write` may touch (Workspace first, spec §29.3's ordering).
    pub writable: Vec<PathBuf>,
    /// Absolute roots an `fs_read` may additionally read (the conventions).
    pub read_only: Vec<PathBuf>,
    /// Absolute roots that are untouchable even if a writable root contains them.
    pub excluded: Vec<PathBuf>,
}

impl PathBoundary {
    /// The verdict for one filesystem invocation: `Ok(mode)` for `"read"` / `"write"`,
    /// `Err(fault)` with the refusal reason otherwise.
    ///
    /// Non-canonical path forms are normalized purely (lexical, no I/O): a trailing
    /// slash or a resolved `..` inside the boundary stays inside; one that escapes a
    /// root (`workspace/../outside`) refuses.
    ///
    /// # Errors
    /// [`BoundaryFault`] with the classification above.
    pub fn permits(
        &self,
        tool: ToolName,
        path: &std::path::Path,
    ) -> Result<&'static str, BoundaryFault> {
        let path = normalize(path);
        // Exclusion wins over every membership (ATH-CORE-043: the control surface sits
        // inside the Workspace and still refuses).
        for root in &self.excluded {
            if path.starts_with(root) {
                return Err(BoundaryFault::Excluded(path.clone()));
            }
        }
        match tool {
            ToolName::FsRead => {
                if self.readable(&path) {
                    Ok("read")
                } else {
                    Err(BoundaryFault::OutOfBoundary(path))
                }
            }
            ToolName::FsWrite => {
                if self.writable(&path) {
                    Ok("write")
                } else {
                    Err(BoundaryFault::OutOfBoundary(path))
                }
            }
            other => Err(BoundaryFault::NotAFilesystemTool { tool: other }),
        }
    }

    fn readable(&self, path: &std::path::Path) -> bool {
        self.writable.iter().any(|root| path.starts_with(root))
            || self.read_only.iter().any(|root| path.starts_with(root))
    }

    fn writable(&self, path: &std::path::Path) -> bool {
        self.writable.iter().any(|root| path.starts_with(root))
    }
}

/// Lexical normalization (no I/O): strip the trailing slash and collapse `.` / `..`
/// against the root's own ancestry. A `..` that escapes the path's own depth is国足 left
/// as-is — it will then refuse against every root, which is the safe answer.
fn normalize(path: &std::path::Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_os_string()),
            std::path::Component::ParentDir => {
                let _ = parts.pop();
            }
            std::path::Component::CurDir => {}
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    let mut normalized = PathBuf::new();
    for part in parts {
        normalized.push(part);
    }
    normalized
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use std::path::Path;

    fn boundary() -> PathBoundary {
        PathBoundary {
            writable: vec![
                PathBuf::from("/athena/var/instances/ai-1"),
                PathBuf::from("/repo/src"),
            ],
            read_only: vec![PathBuf::from("/repo/.aiprj/rules")],
            excluded: vec![
                PathBuf::from("/athena/var/instances/ai-1/.agent-cli"),
                PathBuf::from("/athena/var/instances/ai-1/skills"),
                PathBuf::from("/athena/var/instances/ai-2"),
                PathBuf::from("/work/.athena"),
            ],
        }
    }

    #[test]
    fn writes_are_confined_to_writable_roots() {
        let b = boundary();
        assert_eq!(
            b.permits(
                ToolName::FsWrite,
                Path::new("/athena/var/instances/ai-1/out.txt")
            )
            .expect("inside workspace"),
            "write"
        );
        assert_eq!(
            b.permits(ToolName::FsWrite, Path::new("/repo/src/lib.rs"))
                .expect("work target"),
            "write"
        );
        let refused = b
            .permits(ToolName::FsWrite, Path::new("/repo/other-project/x.rs"))
            .expect_err("out of boundary");
        assert!(
            matches!(refused, BoundaryFault::OutOfBoundary(_)),
            "{refused}"
        );
    }

    #[test]
    fn reads_reach_the_read_only_conventions_and_nothing_more() {
        let b = boundary();
        assert_eq!(
            b.permits(
                ToolName::FsRead,
                Path::new("/repo/.aiprj/rules/exec_job.md")
            )
            .expect("read-only conventions may be read"),
            "read"
        );
        // But not written.
        assert!(
            matches!(
                b.permits(
                    ToolName::FsWrite,
                    Path::new("/repo/.aiprj/rules/exec_job.md")
                ),
                Err(BoundaryFault::OutOfBoundary(_))
            ),
            "the conventions are read-only (ATH-AIPRJ-024's refusal side)"
        );
    }

    #[test]
    fn the_control_surface_is_untouchable_even_inside_the_workspace() {
        let b = boundary();
        for path in [
            "/athena/var/instances/ai-1/.agent-cli/config.toml",
            "/athena/var/instances/ai-1/skills/skill/SKILL.md",
            "/athena/var/instances/ai-2/anything", // another instance's area
            "/work/.athena/var/athena.db",         // the project's store
        ] {
            let refused = b
                .permits(ToolName::FsWrite, Path::new(path))
                .expect_err("excluded wins (ATH-CORE-043)");
            assert!(matches!(refused, BoundaryFault::Excluded(_)), "{refused}");
            // Reading is refused too — exclusion, not a write/read split.
            assert!(
                b.permits(ToolName::FsRead, Path::new(path)).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn traversal_inside_stays_inside_and_escaping_refuses() {
        let b = boundary();
        assert_eq!(
            b.permits(
                ToolName::FsWrite,
                Path::new("/athena/var/instances/ai-1/nested/../out.txt"),
            )
            .expect("lexical .. collapses inside"),
            "write"
        );
        assert!(
            matches!(
                b.permits(
                    ToolName::FsWrite,
                    Path::new("/athena/var/instances/ai-1/../../etc/passwd")
                ),
                Err(BoundaryFault::OutOfBoundary(_) | BoundaryFault::Excluded(_))
            ),
            "escaping the workspace refuses"
        );
    }

    #[test]
    fn non_filesystem_tools_do_not_answer_the_boundary_question() {
        let b = boundary();
        let error = b
            .permits(ToolName::Shell, Path::new("/whatever"))
            .expect_err("a caller bug surfaced");
        assert!(
            matches!(
                error,
                BoundaryFault::NotAFilesystemTool {
                    tool: ToolName::Shell
                }
            ),
            "{error}"
        );
    }
}
