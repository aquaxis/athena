//! The skills conversion bridge (P5-13's API face): a thin wrap of the
//! `athena-skills` shapes — `athena-core` is the crate whose §32.2 edges reach both,
//! so the API's manifest-carrying register body converts here.

/// Convert one manifest YAML text into the Catalog record (registration shape:
/// `AVAILABLE` at the given stamp). Errors surface whole (`ATH-SKILL-009`'s gate
/// happens later, on the disk validation run — this face is the field mapping).
///
/// # Errors
/// [`athena_skills::manifest::ManifestFault`], surfaced.
pub fn manifest_to_record(
    yaml_text: &str,
) -> Result<athena_types::Skill, athena_skills::manifest::ManifestFault> {
    let manifest = athena_skills::manifest::Manifest::parse(yaml_text)?;
    manifest.assemble(athena_types::SkillState::Available, &crate::now_rfc3339())
}
