//! JSON Schema for the Core Control Protocol (ATH-CORE-039, D-4).
//!
//! **[D-4]** the schema is *generated from* the Rust `CoreRequest` type, so the schema and
//! the deserializer cannot drift. The generated schema is committed under version control
//! and diffed in CI (`the_committed_schema_matches_the_code`); a payload-structure change
//! that forgets the schema refresh fails the suite and, with it, CI.
//!
//! Validation (R-CORE-26): Core Host validates the agent's JSON against this schema before
//! anything else inspects the payload. [`ValidationGate`] compiles the schema once, counts
//! the re-emission attempts (ATH-CORE-040: default 3), and escalates the Task as
//! `InternalError` when the limit passes.

use crate::protocol::{CoreRequest, ErrorClass};
use schemars::schema::RootSchema;
use serde_json::Value;

/// The generated §7.5.5 JSON Schema for `CoreRequest`, as a value.
#[must_use]
pub fn core_request_schema() -> RootSchema {
    schemars::schema_for!(CoreRequest)
}

/// The generated schema serialized for transport / commit, pretty-printed with a trailing
/// newline so the committed file stays diff-friendly.
#[must_use]
pub fn core_request_schema_json() -> String {
    let mut text = serde_json::to_string_pretty(&core_request_schema())
        .expect("the generated schema always serializes");
    text.push('\n');
    text
}

/// Where the committed schema lives (version-controlled with the code that generates it).
#[must_use]
pub fn committed_schema_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/core-request.schema.json")
}

/// One candidate payload, checked and typed.
#[derive(Debug, Clone, PartialEq)]
pub enum GateOutcome {
    /// The payload is a valid `CoreRequest` (schema and deserializer agree).
    Accepted(CoreRequest),
    /// The payload failed validation; re-request with the error text attached
    /// (ATH-CORE-040). `re_emissions_left` is how many re-emissions remain before
    /// escalation.
    Rejected {
        /// The validation error text, verbatim, for the agent's next attempt.
        error: String,
        /// Re-emissions still allowed before escalation.
        re_emissions_left: usize,
    },
    /// The re-emission limit passed; the Task escalates as `InternalError`
    /// (ATH-CORE-040, R-CORE-26).
    Escalated {
        /// The final validation error text, for the escalation record.
        error: String,
    },
}

/// The result of one gate submission.
pub type GateResult = std::result::Result<GateOutcome, String>;

/// The bounded validator Core Host runs on every `CoreRequest` payload.
pub struct ValidationGate {
    schema: jsonschema::Validator,
    max_re_emissions: usize,
    accepted: Option<CoreRequest>,
}

impl ValidationGate {
    /// A gate against the current generated schema.
    ///
    /// # Errors
    /// Compilation failure of the generated schema — a build bug surfaced loudly rather
    /// than silently accepting anything.
    pub fn new() -> Result<Self, String> {
        Self::with_limit(3)
    }

    /// A gate with an explicit re-emission ceiling (default 3, ATH-CORE-040).
    ///
    /// # Errors
    /// Compilation failure of the generated schema.
    pub fn with_limit(max_re_emissions: usize) -> Result<Self, String> {
        let document = serde_json::to_value(core_request_schema())
            .map_err(|err| format!("schema did not serialize: {err}"))?;
        let schema = jsonschema::Validator::new(&document)
            .map_err(|err| format!("generated schema did not compile: {err}"))?;
        Ok(Self {
            schema,
            max_re_emissions,
            accepted: None,
        })
    }

    /// Submit one candidate payload.
    ///
    /// Counting: the first invalid submission leaves `max_re_emissions` re-emissions; each
    /// subsequent invalid submission consumes one. The submission that arrives after the
    /// last allowed re-emission was consumed produces [`GateOutcome::Escalated`] instead of
    /// another `Rejected`.
    pub fn attempt(&mut self, payload_json: &str) -> GateResult {
        if self.accepted.is_some() {
            return Err("this gate already accepted a payload; build a new one".into());
        }

        let parsed: Result<Value, _> = serde_json::from_str(payload_json);
        let parsed = match parsed {
            Ok(value) => value,
            Err(err) => return self.reject(format!("payload is not JSON: {err}")),
        };

        let schema_text = match self.schema.iter_errors(&parsed).next() {
            None => Ok(()),
            Some(err) => Err(format!("schema validation failed: {err}")),
        };
        match schema_text {
            Err(text) => self.reject(text),
            Ok(()) => match serde_json::from_value::<CoreRequest>(parsed) {
                Ok(request) => {
                    self.accepted = Some(request.clone());
                    Ok(GateOutcome::Accepted(request))
                }
                Err(err) => {
                    // Schema and deserializer are generated from the same type (D-4); if
                    // they ever disagree, that is a Core-side bug, surfaced as
                    // `InternalError` for the caller.
                    self.reject(format!(
                        "schema accepted, deserializer refused (schema/deserializer drift, \
                         D-4 — Core-side defect): {err}"
                    ))
                }
            },
        }
    }

    fn reject(&mut self, error: String) -> GateResult {
        if self.max_re_emissions == 0 {
            return Ok(GateOutcome::Escalated { error });
        }
        self.max_re_emissions -= 1;
        Ok(GateOutcome::Rejected {
            re_emissions_left: self.max_re_emissions,
            error,
        })
    }

    /// The escalation classification for an exhausted gate: always `InternalError`,
    /// never retried (ATH-FAIL-002).
    #[must_use]
    pub const fn exhausted_class() -> ErrorClass {
        ErrorClass::InternalError
    }
}

impl Default for ValidationGate {
    fn default() -> Self {
        Self::new().expect("the generated schema compiles")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use athena_types::{TaskId, Understanding};

    /// A valid `submit_understanding` payload for the gate.
    fn understanding_payload(goal: &str) -> String {
        serde_json::json!({
            "kind": "submit_understanding",
            "task_id": TaskId::new(),
            "understanding": Understanding {
                goal: goal.into(),
                constraints: vec![],
                completion_criteria: vec![],
                unknowns: vec![],
                risk_level: athena_types::RiskLevel::High,
            },
        })
        .to_string()
    }

    #[test]
    fn the_generated_schema_accepts_the_protocol_on_the_wire() {
        // Every one of the twelve kinds validates against the generated schema — same
        // source as the deserializer, so agreement is the point of the check (D-4).
        for (payload, kind) in crate::protocol_tests::wire_sample_payloads() {
            let gate = &mut ValidationGate::new().expect("compiles");
            let outcome = gate
                .attempt(&payload)
                .unwrap_or_else(|err| panic!("gate reachable for {kind}: {err}"));
            assert!(
                matches!(outcome, GateOutcome::Accepted(_)),
                "schema must accept {kind}: {payload}"
            );
        }
    }

    /// Regenerate the committed schema: `cargo test -p athena-core -- --ignored regenerate_schema`.
    ///
    /// Deliberately `#[ignore]`ed so ordinary runs diff rather than rewrite (the CI gate
    /// must *reject* drift, not paper over it).
    #[test]
    #[ignore = "regenerate the committed schema intentionally"]
    fn regenerate_schema() {
        let _ =
            std::fs::create_dir_all(committed_schema_path().parent().expect("parent directory"));
        std::fs::write(committed_schema_path(), core_request_schema_json())
            .expect("committed schema writes");
    }

    #[test]
    fn the_schema_is_stable_and_committed_in_version_control() {
        // ATH-CORE-039 + D-4: the committed schema is the generated one, byte for byte.
        let generated = core_request_schema_json();
        let committed =
            std::fs::read_to_string(committed_schema_path()).expect("committed schema exists");
        assert_eq!(
            generated, committed,
            "the committed schema is stale; regenerate it from the current CoreRequest type"
        );
    }

    #[test]
    fn an_malformed_payload_is_rejected_with_the_re_emission_budget() {
        let mut gate = ValidationGate::new().expect("compiles");
        let outcome = gate.attempt("not json at all").expect("gate reachable");
        assert!(
            matches!(
                outcome,
                GateOutcome::Rejected {
                    re_emissions_left: 2,
                    ..
                }
            ),
            "{outcome:?}",
        );
    }

    #[test]
    fn out_of_protocol_kinds_are_refused_by_the_schema() {
        // ATH-CORE-033: no side path may exist; an unknown kind cannot pass the schema.
        let mut gate = ValidationGate::new().expect("compiles");
        let payload = serde_json::json!({
            "kind": "start_agent_directly",
            "agent_id": "x",
        })
        .to_string();
        let outcome = gate.attempt(&payload).expect("gate reachable");
        assert!(
            matches!(outcome, GateOutcome::Rejected { .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn the_re_emission_limit_escalates_instead_of_retrying_forever() {
        // ATH-CORE-040: initial submission + 3 re-emissions, then InternalError + escalation.
        let payloads = [
            invalid_payload(),
            invalid_payload(),
            invalid_payload(),
            invalid_payload(),
        ];
        let mut gate = ValidationGate::new().expect("compiles");
        let first = gate.attempt(&payloads[0]).expect("gate reachable");
        assert!(
            matches!(
                &first,
                GateOutcome::Rejected {
                    re_emissions_left: 2,
                    ..
                }
            ),
            "{first:?}"
        );
        let second = gate.attempt(&payloads[1]).expect("gate reachable");
        assert!(
            matches!(
                &second,
                GateOutcome::Rejected {
                    re_emissions_left: 1,
                    ..
                }
            ),
            "{second:?}"
        );
        let third = gate.attempt(&payloads[2]).expect("gate reachable");
        assert!(
            matches!(
                &third,
                GateOutcome::Rejected {
                    re_emissions_left: 0,
                    ..
                }
            ),
            "{third:?}"
        );
        // The fourth invalid payload was already past every re-emission: escalate.
        let fourth = gate.attempt(&payloads[3]).expect("gate reachable");
        assert!(
            matches!(&fourth, GateOutcome::Escalated { .. }),
            "{fourth:?}"
        );
        assert_eq!(ValidationGate::exhausted_class(), ErrorClass::InternalError);
    }

    #[test]
    fn a_valid_payload_within_the_budget_is_accepted_and_ends_the_gate() {
        // Three failures, then a fixed payload: acceptance uses the last re-emission.
        let mut gate = ValidationGate::new().expect("compiles");
        for _ in 0..3 {
            let outcome = gate.attempt(&invalid_payload()).expect("gate reachable");
            assert!(matches!(outcome, GateOutcome::Rejected { .. }));
        }
        let outcome = gate
            .attempt(&understanding_payload("late but right"))
            .expect("gate reachable");
        let accepted = match outcome {
            GateOutcome::Accepted(request) => request,
            other => panic!("{other:?}"),
        };
        let understanding = match accepted {
            CoreRequest::SubmitUnderstanding { understanding, .. } => understanding.goal,
            _ => panic!("wrong kind"),
        };
        assert_eq!(understanding, "late but right");

        // Another payload after an accepted one is refused outright.
        let err = gate
            .attempt(&understanding_payload("second"))
            .expect_err("gate closed");
        assert!(err.contains("already accepted"), "{err}");
    }

    #[test]
    fn the_draft_stepshape_stays_identifier_free() {
        // D-5 regression at the schema level: the submit_plan step object carries no `id`.
        let schema = core_request_schema_json();
        let value: Value = serde_json::from_str(&schema).expect("valid");
        let serialized = value.to_string();
        // A naive-but-effective check: the plan step definition must not expose an `id`
        // property of its own. The schema for PlanStepDraft lives in `definitions`.
        let definitions = value.get("definitions").expect("definitions exist");
        let plan_step_draft = definitions
            .get("PlanStepDraft")
            .expect("PlanStepDraft definition");
        let props = plan_step_draft.get("properties").expect("properties");
        assert!(
            props.get("id").is_none() && props.get("plan_step_id").is_none(),
            "{props}",
        );
        assert!(serialized.contains("order") && serialized.contains("intent"));
    }

    fn invalid_payload() -> String {
        let mut payload = understanding_payload("wrong shape");
        // Corrupt the payload structurally so the schema refuses it.
        let mut value: Value = serde_json::from_str(&payload).expect("json");
        let object = value.as_object_mut().expect("object");
        object.insert("kind".to_string(), Value::String("submit_plan".into()));
        object.remove("understanding");
        payload = value.to_string();
        payload
    }
}
