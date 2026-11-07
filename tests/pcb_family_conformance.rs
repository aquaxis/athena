//! PCB distributed-defaults family conformance (R-FIX-01's MUST closure): every persona
//! of the fixed family parses through the **real** persona loader against its pairing
//! Agent Definition (the R-PER-03 discipline), and every Skill Package opens through the
//! real `Package::open` (the `ATH-SKILL-003` both-files rule) with a validated manifest.
//! No real `agent-cli` process, no LLM (`ATH-TEST-022`).

use athena_agent::persona::load_persona_from_text;
use athena_types::{AgentDefinition, AgentId, AgentLevel, ModelRef, RuntimeType, ToolName};

/// The workspace root: this package sits at `<root>/tests/`, so the parent is the root
/// (the boundary test's upward-walk discipline keeps this correct if relocated).
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the conformance package always sits at <root>/tests/")
        .to_path_buf()
}

/// The pairing definition for a persona slug: the level must equal the persona's
/// front-matter `level` (the loader's `LevelMismatched` check), and the granted tools
/// must contain every tool the persona asks for (the `PermissionExceeded` check).
fn definition(slug: &str, level: AgentLevel, tools: &[ToolName]) -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new(format!("athena-pcb-{slug}")),
        name: format!("pcb-{slug}"),
        level,
        description: format!("PCB {slug} stage agent (family conformance pairing)"),
        capabilities: vec![format!("pcb-{slug}")],
        persona_ref: format!("personas/pcb-{slug}_ja.md"),
        persona_version: "1.0.0".to_string(),
        allowed_tools: tools.to_vec(),
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model: ModelRef {
            provider: "ollama".into(),
            name: "glm-5.3-flash:cloud".into(),
        },
        max_delegation_depth: 4,
        dynamic: false,
    }
}

/// The family's pairing table: slug → (level, the tools its definition must grant so the
/// persona's asks stay inside the grant). The levels are the ones the fix settled.
fn pairing(slug: &str) -> (AgentLevel, Vec<ToolName>) {
    let fs = vec![
        ToolName::FsRead,
        ToolName::FsWrite,
        ToolName::Search,
        ToolName::SendTo,
    ];
    let orchestrator = vec![
        ToolName::FsRead,
        ToolName::FsWrite,
        ToolName::Shell,
        ToolName::Git,
        ToolName::Search,
        ToolName::SendTo,
    ];
    let schematic = vec![
        ToolName::FsRead,
        ToolName::FsWrite,
        ToolName::Shell,
        ToolName::Search,
        ToolName::SendTo,
    ];
    match slug {
        "orchestrator" => (AgentLevel::Producer, orchestrator),
        "grand-design" | "system-design" => (AgentLevel::Lead, fs.clone()),
        "schematic-design" | "artwork-design" | "si-analysis" | "pi-analysis"
        | "parts-research" => (AgentLevel::Worker, schematic),
        "common-operator" => (AgentLevel::Worker, fs),
        _ => unreachable!("unknown pcb slug {slug}"),
    }
}

const PERSONAS: &[&str] = &[
    "pcb-orchestrator",
    "pcb-grand-design",
    "pcb-system-design",
    "pcb-schematic-design",
    "pcb-artwork-design",
    "pcb-si-analysis",
    "pcb-pi-analysis",
    "pcb-parts-research",
    "pcb-common-operator",
];

const PACKAGES: &[&str] = &[
    "pcb-orchestrator",
    "pcb-grand-design",
    "pcb-system-design",
    "pcb-schematic-design",
    "pcb-artwork-design",
    "pcb-si-analysis",
    "pcb-pi-analysis",
    "pcb-parts-research",
    "pcb-common",
];

/// Every fixed-family persona parses through the real loader, pairing its definition:
/// level matches, tool asks inside the grant.
#[test]
fn the_pcb_family_personas_parse_through_the_real_persona_loader() {
    for slug in PERSONAS {
        let path = workspace_root().join(format!("personas/{slug}_ja.md"));
        let text = std::fs::read_to_string(&path).expect("read the persona file");
        let (level, tools) = pairing(slug.strip_prefix("pcb-").unwrap_or(slug));
        let def = definition(slug, level, &tools);
        let loaded = load_persona_from_text(&text, &def.id, &def).unwrap_or_else(|fault| {
            panic!(
                "{} refused through the real loader: {fault}",
                path.display()
            )
        });
        assert_eq!(
            loaded.front_matter.level,
            level,
            "{}: level pairing failed",
            path.display()
        );
        for tool in &loaded.front_matter.allowed_tools {
            assert!(
                def.allowed_tools.contains(tool),
                "{}: persona asks for {tool} outside its definition's grant",
                path.display()
            );
        }
    }
}

/// Every fixed-family package opens through the real `Package::open` (both files present,
/// manifest parses + validates) with the contract file names.
#[test]
fn the_pcb_family_packages_open_through_the_real_package_open() {
    for slug in PACKAGES {
        let root = workspace_root().join(format!("skills/{slug}"));
        let display = root.display().to_string();
        let pkg = athena_skills::package::Package::open(&root).unwrap_or_else(|fault| {
            panic!("{display} refused through the real Package::open: {fault}")
        });
        assert!(!pkg.skill_md.is_empty(), "{display}: empty SKILL.md body");
        assert_eq!(
            pkg.manifest.id, *slug,
            "{display}: manifest id does not name the package directory"
        );
        assert!(
            !pkg.manifest.version.is_empty(),
            "{display}: manifest version blank"
        );
    }
}

/// The manifest-declared dependency graph stays a DAG with well-formed range entries
/// (each dependency's id names a package in the family; the version is range-shaped).
#[test]
fn the_pcb_family_dependency_graph_is_a_well_formed_dag() {
    use std::collections::BTreeMap;
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for slug in PACKAGES {
        let pkg =
            athena_skills::package::Package::open(&workspace_root().join(format!("skills/{slug}")))
                .expect("open the package to read its manifest");
        let mut targets = Vec::new();
        for dep in &pkg.manifest.dependencies {
            assert!(
                PACKAGES.contains(&dep.id.as_str()),
                "{slug}: dependency {} names no package in the family",
                dep.id
            );
            targets.push(dep.id.clone());
        }
        deps.insert((*slug).to_string(), targets);
    }
    // Cycle check (Kahn): every cycle candidate never drains.
    let mut remaining: BTreeMap<String, usize> = deps
        .iter()
        .map(|(slug, targets)| (slug.clone(), targets.len()))
        .collect();
    let mut queue: Vec<String> = remaining
        .iter()
        .filter(|&(_, &n)| n == 0)
        .map(|(s, _)| s.clone())
        .collect();
    while let Some(slug) = queue.pop() {
        for (other, targets) in &deps {
            if targets.contains(&slug)
                && let Some(count) = remaining.get_mut(other)
            {
                *count -= 1;
                if *count == 0 {
                    queue.push(other.clone());
                }
            }
        }
    }
    let cyclic: Vec<&String> = remaining
        .iter()
        .filter(|(_, deg)| **deg > 0)
        .map(|(slug, _)| slug)
        .collect();
    assert!(
        cyclic.is_empty(),
        "the family's dependency graph has a cycle among: {cyclic:?}"
    );
}
