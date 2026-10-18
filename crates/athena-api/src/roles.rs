//! The role hierarchy (spec §29.2, ATH-SEC-003).

/// The three roles; each strictly above the previous (spec §29.2 ordering).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Read-only (`viewer`).
    Viewer,
    /// `viewer` + Task create / run / cancel, approval answer.
    Operator,
    /// `operator` + Skill management, config inspection, forced stop.
    Admin,
}

impl Role {
    /// The wire form (`viewer` / `operator` / `admin`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Operator => "operator",
            Self::Admin => "admin",
        }
    }

    /// Whether the caller's role satisfies an endpoint's requirement (spec §29.2: each
    /// role subsumes the previous).
    #[must_use]
    pub const fn satisfies(self, required: Self) -> bool {
        (self as u8) >= (required as u8)
    }

    /// Every role, for table-driven tests.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Viewer, Self::Operator, Self::Admin]
    }
}

#[cfg(test)]
#[path = "roles_tests.rs"]
mod roles_tests;
