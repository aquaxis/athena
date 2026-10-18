//! The `manifest.yaml` parser — the machine-managed half of a Skill Package.
//!
//! The manifest mirrors §12.3's Catalog shape field for field: of the fifteen fields
//! `ATH-SKILL-005` names, fourteen are declared here; `status` is lifecycle-managed by
//! the Registry per `(id, version)` (`ATH-STATE-026`), and a checksum the manifest
//! omits falls to the computed content checksum (`ATH-SKILL-036`).
//!
//! Unknown keys are refused (`deny_unknown_fields`): a typo'd manifest must not silently
//! lose a metadata field, because the Catalog is the only thing the Core Agent reads
//! (`ATH-SKILL-002` — it never opens the package itself).

use athena_types::{NetworkMode, Permission, RiskLevel, SkillState, ToolName};
use serde::Deserialize;

/// Why a manifest was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestFault {
    /// The YAML did not parse, or a structured grant was not shapable.
    #[error("invalid manifest.yaml: {0}")]
    Yaml(String),
    /// A required field is missing or blank (`ATH-SKILL-005`: every field present).
    #[error("manifest field {0:?} must be non-empty")]
    BlankField(&'static str),
    /// An unknown tool name — the Catalog's `required_tools` bind the packed tool demand,
    /// so a name outside the §11 tool enum cannot be honored.
    #[error("unknown required_tool {0:?}")]
    UnknownTool(String),
    /// The declared version is not a SemVer-shaped version (`ATH-SKILL-019`, `ATH-SKILL-028`).
    #[error("manifest version {0:?} is not a semantic version (MAJOR.MINOR.PATCH)")]
    BadVersion(String),
    /// A dependency entry is malformed.
    #[error("manifest dependency {0:?} is malformed (needs id and a version range)")]
    BadDependency(String),
}

/// One dependency entry: the Skill id plus a SemVer range (`ATH-SKILL-028`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    /// The dependency's Skill id.
    pub id: String,
    /// The SemVer range (`^1.0.0` et al.).
    pub version: String,
}

/// The `runtime:` block.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    /// The provider (`agent-cli` in v1).
    pub provider: String,
}

/// The `risk:` block.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Risk {
    /// `low` / `medium` / `high` (`high` requires approval to use, `ATH-SKILL-024`).
    pub level: String,
}

/// The `fs:` structured grant body (`fs: {mode, scope}`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsGrant {
    /// Read or write.
    pub mode: String,
    /// The path scope.
    pub scope: String,
}

/// The `network:` structured grant body (`network: {mode, hosts}`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkGrant {
    /// The access mode (`allow` / `deny` / `allowlist`).
    pub mode: String,
    /// Host allow-list when the mode allows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<String>>,
}

/// A permission entry in the manifest: either a bare tool name (`- shell`) or a
/// structured single-key grant (`fs: {…}` / `network: {…}` / `process:`).
/// Untagged for YAML: the entry's own shape discriminates it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionSpec {
    /// A bare tool permission name (`shell`, `fs_read`, …).
    Tool(String),
    /// Filesystem access within a path scope.
    Fs {
        /// The `fs:` grant body.
        fs: FsGrant,
    },
    /// Network access.
    Network {
        /// The `network:` grant body.
        network: NetworkGrant,
    },
    /// Process manipulation (`process:` with any value).
    Process {
        /// The `process:` value (unused).
        #[serde(default)]
        process: Option<String>,
    },
}

/// The parsed `manifest.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The Skill slug.
    pub id: String,
    /// The display name.
    pub name: String,
    /// The SemVer version (pinned at agent start, `ATH-SKILL-020`).
    pub version: String,
    /// The one-line description.
    pub description: String,
    /// Capabilities offered.
    pub capabilities: Vec<String>,
    /// Tools the skill demands (the §11 grant checks name these).
    pub required_tools: Vec<String>,
    /// Permissions required; checked as a union over the dependency closure
    /// (`ATH-SKILL-031`, `ATH-SEC-012`).
    pub required_permissions: Vec<PermissionSpec>,
    /// Agents this skill supports.
    pub supported_agents: Vec<String>,
    /// Dependencies as SemVer ranges.
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    /// The runtime provider.
    pub runtime: Runtime,
    /// The origin (external skills must come from the allow-list, `ATH-SKILL-022/036`).
    pub source: String,
    /// The declared checksum; when absent, the Validator computes the content checksum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    /// The publisher (`ATH-SKILL-036`'s second trust basis).
    pub publisher: String,
    /// The risk block.
    pub risk: Risk,
    /// Whether hot reload is declared safe (`ATH-SKILL-011`; `false` by default).
    #[serde(default)]
    pub hot_reload_safe: bool,
}

impl Manifest {
    /// Parse a manifest. Strict: unknown keys, missing required fields, blank content,
    /// non-SemVer versions, unknown tool names, and malformed dependencies are faults —
    /// the Catalog must be exactly what the §12.3 shape declares (`ATH-SKILL-005`).
    ///
    /// # Errors
    /// [`ManifestFault`] with the first problem found, in field order.
    pub fn parse(yaml: &str) -> Result<Self, ManifestFault> {
        let manifest: Manifest = serde_yaml::from_str(yaml).map_err(|error| {
            ManifestFault::Yaml(
                error
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            )
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Post-parse validation: the fields `ATH-SKILL-005` names must carry real content,
    /// the version must be a SemVer-shaped `MAJOR.MINOR.PATCH`, every declared tool must
    /// be a §11 tool, and every dependency must carry a version range.
    ///
    /// # Errors
    /// [`ManifestFault`] with the first problem found, in field order.
    pub fn validate(&self) -> Result<(), ManifestFault> {
        for (field, value) in [
            ("id", self.id.as_str()),
            ("name", self.name.as_str()),
            ("version", self.version.as_str()),
            ("description", self.description.as_str()),
            ("source", self.source.as_str()),
            ("publisher", self.publisher.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ManifestFault::BlankField(field));
            }
        }
        if !Self::is_semver(&self.version) {
            return Err(ManifestFault::BadVersion(self.version.clone()));
        }
        for name in &self.required_tools {
            parse_tool(name)?;
        }
        parse_risk(&self.risk.level)?;
        for dependency in &self.dependencies {
            if dependency.id.trim().is_empty()
                || dependency.version.trim().is_empty()
                || !Self::is_range(&dependency.version)
            {
                return Err(ManifestFault::BadDependency(format!(
                    "{}={}",
                    dependency.id, dependency.version
                )));
            }
        }
        Ok(())
    }

    /// A strict SemVer shape: `MAJOR.MINOR.PATCH` leading, an optional suffix after.
    #[must_use]
    pub fn is_semver(version: &str) -> bool {
        let head = version.split(['-', '+']).next().unwrap_or(version);
        let parts: Vec<&str> = head.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }

    /// A version-range shape: any version prefix bound (`^`, `~`, `>=`, `=`, or a plain
    /// SemVer, including partial floors like `~2.3`). The Resolver (P5-06) owns
    /// satisfaction; the manifest only declares.
    #[must_use]
    pub fn is_range(range: &str) -> bool {
        let stripped = range.trim_start_matches(['^', '~', '=', '>', '<', 'v', ' ']);
        if stripped.starts_with('*') {
            return true;
        }
        // A partial bound (`~2.3`, `^1`) declares a floor: two or three numeric parts.
        let head = stripped.split(['-', '+']).next().unwrap_or(stripped);
        let parts: Vec<&str> = head.split('.').collect();
        (2..=3).contains(&(parts.len() as u64))
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }

    /// The declared tool names as the §11 enum values.
    ///
    /// # Errors
    /// [`ManifestFault::UnknownTool`] for any name outside the tool enum.
    pub fn tool_names(&self) -> Result<Vec<ToolName>, ManifestFault> {
        self.required_tools
            .iter()
            .map(|name| parse_tool(name))
            .collect()
    }

    /// The permission entries as the entity's `Permission` shape.
    ///
    /// # Errors
    /// [`ManifestFault::UnknownTool`] for a bare permission name outside the tool enum,
    /// [`ManifestFault::Yaml`] for a structured grant with an unknown mode.
    pub fn permissions(&self) -> Result<Vec<Permission>, ManifestFault> {
        self.required_permissions
            .iter()
            .map(|spec| match spec {
                PermissionSpec::Tool(name) => {
                    parse_tool(name).map(|tool| Permission::Tool { tool })
                }
                PermissionSpec::Fs {
                    fs: FsGrant { mode, scope },
                } => match mode.as_str() {
                    "read" => Ok(Permission::Fs {
                        mode: athena_types::FsMode::Read,
                        scope: scope.clone(),
                    }),
                    "write" => Ok(Permission::Fs {
                        mode: athena_types::FsMode::Write,
                        scope: scope.clone(),
                    }),
                    other => Err(ManifestFault::Yaml(format!("unknown fs mode {other:?}"))),
                },
                PermissionSpec::Network {
                    network: NetworkGrant { mode, hosts },
                } => match mode.as_str() {
                    "deny" => Ok(Permission::Network {
                        mode: NetworkMode::Deny,
                        hosts: None,
                    }),
                    "allow" => Ok(Permission::Network {
                        mode: NetworkMode::Allow,
                        hosts: None,
                    }),
                    "allowlist" => Ok(Permission::Network {
                        mode: NetworkMode::Allowlist,
                        hosts: hosts.clone(),
                    }),
                    other => Err(ManifestFault::Yaml(format!(
                        "unknown network mode {other:?}"
                    ))),
                },
                PermissionSpec::Process { .. } => Ok(Permission::Process),
            })
            .collect()
    }
}

/// Map a manifest tool name to the §11 tool enum (the wire tokens are the names).
fn parse_tool(name: &str) -> Result<ToolName, ManifestFault> {
    ToolName::ALL
        .iter()
        .copied()
        .find(|tool| tool.as_str() == name)
        .ok_or_else(|| ManifestFault::UnknownTool(name.to_string()))
}

/// Map a manifest risk level to the entity's `RiskLevel`.
fn parse_risk(level: &str) -> Result<RiskLevel, ManifestFault> {
    RiskLevel::ALL
        .iter()
        .copied()
        .find(|risk| risk.as_str() == level)
        .ok_or_else(|| ManifestFault::Yaml(format!("unknown risk level {level:?}")))
}

#[cfg(test)]
mod manifest_tests;

impl Manifest {
    /// Assemble the §12.3 Catalog record from the manifest alone — the no-package face
    /// the API's manifest-carrying body uses (registration lands `AVAILABLE` through the
    /// §17.3 validation pair; the checksum: a manifest-declared value verbatim, an absent
    /// one left zero — the Validator's disk run (P5-02) is what verifies it).
    ///
    /// # Errors
    /// The manifest's own shape faults (already validated at parse; the assemble leg
    /// propagates the tool/permission mapping's faults).
    pub fn assemble(
        &self,
        state: SkillState,
        at: &str,
    ) -> Result<athena_types::Skill, ManifestFault> {
        Ok(athena_types::Skill {
            id: athena_types::SkillId::new(self.id.clone()),
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            capabilities: self.capabilities.clone(),
            required_tools: self.tool_names()?,
            required_permissions: self.permissions()?,
            supported_agents: self
                .supported_agents
                .iter()
                .map(|slug| athena_types::AgentId::new(slug.clone()))
                .collect(),
            dependencies: self
                .dependencies
                .iter()
                .map(|dependency| athena_types::SkillRef {
                    id: athena_types::SkillId::new(dependency.id.clone()),
                    version: dependency.version.clone(),
                    checksum: None,
                })
                .collect(),
            runtime_provider: self.runtime.provider.clone(),
            source: self.source.clone(),
            checksum: self.checksum.clone().unwrap_or_default(),
            publisher: self.publisher.clone(),
            risk_level: athena_types::RiskLevel::ALL
                .iter()
                .copied()
                .find(|risk| risk.as_str() == self.risk.level)
                .unwrap_or(athena_types::RiskLevel::Medium),
            state,
            hot_reload_safe: self.hot_reload_safe,
            registered_at: at.to_string(),
            updated_at: at.to_string(),
        })
    }
}
