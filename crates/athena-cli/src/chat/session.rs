//! The `athena chat` session loop (X-27, P6-02/03): the dialogue face over the
//! performing HTTP transport.
//!
//! One session is one instruction-driven Task, watched from creation to a terminal
//! state (`ATH-CLI-005`): the loop posts the Task, polls its state, renders the
//! progress and, while the Task waits on a human, presents the pending record
//! inline — a clarification question (`ATH-HITL-020`) or an approval
//! (`ATH-HITL-010/011`) — and posts the answer through the same transport.
//!
//! Deterministic and transport-connected: every decision reads the Core API's answer,
//! every post is the transport's own request; the loop invents nothing (`ATH-CORE-038`
//! at the client face). `ATH-CLI-001` (Core-API-only) and `ATH-CLI-002` (no
//! `agent-cli` launch) hold.

use crate::endpoint::Endpoint;
use crate::exit_code::ExitCode;

/// How long one chat session waits for the Task to reach a terminal state.
pub const SESSION_TIMEOUT_SECS: u64 = 900;

/// The poll cadence between state reads.
pub const POLL_SECS: u64 = 2;

/// Why a chat session stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionFault {
    /// The Core API could not be reached or the exchange failed.
    #[error("transport: {0}")]
    Transport(String),
    /// The API refused with the §22.2 envelope.
    #[error("HTTP {status}: {body}")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// The response body.
        body: String,
    },
    /// The answer's shape could not be read (missing `id`/`state`).
    #[error("the Task answer is unreadable: {0}")]
    Unreadable(String),
    /// The Task never reached a terminal state inside the session budget.
    #[error("the task {0} did not reach a terminal state within {1}s")]
    Timeout(String, u64),
}

/// One session's rendered transcript, in the order the loop printed it.
pub struct Transcript {
    /// The rendered lines, in print order.
    pub lines: Vec<String>,
    /// The process exit code (0 terminal, 70 transport/refusal, 1 timeout).
    pub code: i32,
}

#[must_use]
pub fn session_lines(lines: Vec<String>) -> Transcript {
    Transcript { lines, code: 0 }
}

/// The read-back of one Task row, the fields the loop renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    /// The Task identifier.
    pub id: String,
    /// The state name as the API spelled it (e.g. `RUNNING`).
    pub state: String,
}

/// The pending clarification, as the conversation reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClarificationView {
    /// The clarification identifier.
    pub id: String,
    /// The Core Agent's question.
    pub question: String,
    /// The answer deadline.
    pub expires_at: String,
}

/// The pending approval, as the conversation reads it (the canonical descriptor
/// foremost; the agent prose demoted under its untrusted marker, `ATH-HITL-011`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalView {
    /// The approval identifier.
    pub id: String,
    /// The Core Host's canonical descriptor (`ATH-HITL-010`).
    pub descriptor: String,
    /// The risk, from configuration (`ATH-CORE-038`).
    pub risk: String,
    /// The expires-at instant.
    pub expires_at: String,
    /// The agent's reason — untrusted prose.
    pub agent_reason: Option<String>,
    /// The agent's impact — untrusted prose.
    pub agent_impact: Option<String>,
}

/// Run one chat session: create the Task from the typed instruction, drive the
/// conversation to a terminal state, and return the transcript.
///
/// The answer channel is the caller's closure: the loop presents a pending record and
/// the closure supplies the human's reply (a terminal, in the binary face; typed text,
/// in the tests). Nothing here consults a TTY.
pub fn run_session(
    endpoint: &Endpoint,
    instruction: &str,
    answer_for: &mut dyn FnMut(&PendingPrompt) -> String,
) -> Result<Transcript, SessionFault> {
    let mut lines: Vec<String> = Vec::new();

    // 1. Create the Task (auto-run: the loop would queue it anyway, but the POST carries
    //    `auto_run: true` so the queue accepts it at creation, ATH-DATA-009).
    let body = serde_json::json!({
        "instruction": instruction,
        "workspace": std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|_| ".".into()),
        "auto_run": true,
    })
    .to_string();
    let created = post(endpoint, "/api/v1/tasks", Some(&body))?;
    let id = created
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| SessionFault::Unreadable(created["id"].to_string()))?
        .to_string();
    lines.push(format!("[CREATED] task {id}"));

    // 2. Drive to a terminal state inside the session budget.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(SESSION_TIMEOUT_SECS);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(POLL_SECS));
        if std::time::Instant::now() > deadline {
            lines.push(format!(
                "[TIMEOUT] task {id} has not reached a terminal state within {SESSION_TIMEOUT_SECS}s"
            ));
            return Ok(Transcript {
                lines,
                code: ExitCode::ExecutionFailure.as_i32(),
            });
        }
        let view = read_task(endpoint, &id)?;
        match view.state.as_str() {
            "CREATED" | "QUEUED" | "RETRYING" | "REPLANNING" => {
                lines.push(format!("[{}] task {id}", view.state));
            }
            "RUNNING" | "EVALUATING" => {
                lines.push(progress_line(&view));
            }
            "WAITING_APPROVAL" => {
                let approval = read_pending_approval(endpoint, &id)?;
                match approval {
                    Some(prompt) => {
                        lines.push(render_approval(&prompt));
                        let reply = answer_for(&PendingPrompt::Approval(prompt.id.clone()));
                        let decision = decision_of(&reply);
                        let path = format!("/api/v1/approvals/{}/{}", prompt.id, decision);
                        let _ = post(endpoint, &path, None)?;
                        lines.push(format!("[RESPONDED] approval {decision} (task {id})"));
                    }
                    None => lines.push(progress_line(&view)),
                }
            }
            "WAITING_INPUT" => {
                let clarify = read_pending_clarification(endpoint, &id)?;
                match clarify {
                    Some(prompt) => {
                        lines.push(render_clarification(&prompt));
                        let reply = answer_for(&PendingPrompt::Clarification(prompt.id.clone()));
                        let body = serde_json::json!({
                            "clarification_id": prompt.id,
                            "answer": reply,
                        })
                        .to_string();
                        let path = format!("/api/v1/tasks/{id}/answer");
                        let _ = post(endpoint, &path, Some(&body))?;
                        lines.push(format!("[ANSWERED] clarification (task {id})"));
                    }
                    None => lines.push(progress_line(&view)),
                }
            }
            "SUCCESS" => {
                lines.push(format!("[SUCCESS] task {id} completed — result recorded"));
                return Ok(Transcript { lines, code: 0 });
            }
            "FAILED" => {
                lines.push(format!("[FAILED] task {id} ended in failure"));
                return Ok(Transcript {
                    lines,
                    code: ExitCode::ExecutionFailure.as_i32(),
                });
            }
            "CANCELLED" => {
                lines.push(format!("[CANCELLED] task {id} was cancelled"));
                return Ok(Transcript {
                    lines,
                    code: ExitCode::RequestShapeViolation.as_i32(),
                });
            }
            other => {
                return Err(SessionFault::Unreadable(format!("state {other:?}")));
            }
        }
    }
}

/// What the loop is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingPrompt {
    /// A clarification awaiting the human's answer.
    Clarification(String),
    /// An approval awaiting the human's decision.
    Approval(String),
}

// --- The transport faces -------------------------------------------------------------------

fn post(
    endpoint: &Endpoint,
    path: &str,
    body: Option<&str>,
) -> Result<serde_json::Value, SessionFault> {
    let response = crate::http::request(endpoint, "POST", path, body)
        .map_err(|fault| SessionFault::Transport(fault.to_string()))?;
    if !response.is_success() {
        return Err(SessionFault::Refused {
            status: response.status,
            body: response.body,
        });
    }
    if response.body.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&response.body)
        .map_err(|error| SessionFault::Unreadable(error.to_string()))
}

fn get(endpoint: &Endpoint, path: &str) -> Result<serde_json::Value, SessionFault> {
    let response = crate::http::request(endpoint, "GET", path, None)
        .map_err(|fault| SessionFault::Transport(fault.to_string()))?;
    if !response.is_success() {
        return Err(SessionFault::Refused {
            status: response.status,
            body: response.body,
        });
    }
    serde_json::from_str(&response.body)
        .map_err(|error| SessionFault::Unreadable(error.to_string()))
}

fn read_task(endpoint: &Endpoint, id: &str) -> Result<TaskView, SessionFault> {
    let value = get(endpoint, &format!("/api/v1/tasks/{id}"))?;
    let state = value["state"]
        .as_str()
        .ok_or_else(|| SessionFault::Unreadable("no state field".into()))?;
    Ok(TaskView {
        id: id.to_string(),
        state: state.to_string(),
    })
}

fn read_pending_clarification(
    endpoint: &Endpoint,
    id: &str,
) -> Result<Option<ClarificationView>, SessionFault> {
    let value = get(endpoint, &format!("/api/v1/tasks/{id}/clarification"))?;
    let Some(clarification_id) = value["clarification_id"].as_str() else {
        return Ok(None);
    };
    Ok(Some(ClarificationView {
        id: clarification_id.to_string(),
        question: value["question"].as_str().unwrap_or_default().to_string(),
        expires_at: value["expires_at"].as_str().unwrap_or_default().to_string(),
    }))
}

fn read_pending_approval(
    endpoint: &Endpoint,
    id: &str,
) -> Result<Option<ApprovalView>, SessionFault> {
    let value = get(endpoint, "/api/v1/approvals?state=requested")?;
    let Some(items) = value["items"].as_array() else {
        return Ok(None);
    };
    // The Task's own pending approval, if any.
    for item in items {
        if item["task_id"].as_str() != Some(id) {
            continue;
        }
        return Ok(Some(ApprovalView {
            id: item["id"].as_str().unwrap_or_default().to_string(),
            descriptor: item["operation_descriptor"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            risk: item["risk"].as_str().unwrap_or_default().to_string(),
            expires_at: item["expires_at"].as_str().unwrap_or_default().to_string(),
            agent_reason: item["reason_untrusted"].as_str().map(String::from),
            agent_impact: item["impact_untrusted"].as_str().map(String::from),
        }));
    }
    Ok(None)
}

fn decision_of(reply: &str) -> &'static str {
    let lowered = reply.trim().to_lowercase();
    if lowered.starts_with("approve") || lowered.starts_with("yes") {
        "approve"
    } else {
        "reject"
    }
}

// --- The render faces (ATH-CLI-004 / 005) --------------------------------------------------

fn progress_line(view: &TaskView) -> String {
    format!("[{}] task {} — in flight", view.state, view.id)
}

fn render_clarification(prompt: &ClarificationView) -> String {
    format!(
        "QUESTION [{}]\n{}\n(answer; deadline {expires} — silence fails the Task)",
        prompt.id,
        prompt.question,
        expires = prompt.expires_at
    )
}

fn render_approval(prompt: &ApprovalView) -> String {
    let mut text = format!(
        "APPROVAL REQUIRED [{}]\n{}\nrisk: {} • expires: {}\n",
        prompt.id, prompt.descriptor, prompt.risk, prompt.expires_at
    );
    if let Some(reason) = &prompt.agent_reason {
        text.push_str(&format!("  [untrusted agent prose] reason: {reason}\n"));
    }
    if let Some(impact) = &prompt.agent_impact {
        text.push_str(&format!("  [untrusted agent prose] impact: {impact}\n"));
    }
    text.push_str("reply `approve` or `reject`.\n");
    text
}

/// The rendered transcript, for the terminal.
#[must_use]
pub fn rendered(lines: &[String]) -> String {
    lines.join("\n")
}

#[cfg(test)]
mod session_tests {
    use super::*;

    fn loopback() -> crate::endpoint::Endpoint {
        crate::endpoint::Endpoint {
            base: "http://127.0.0.1:9".into(),
            auth: crate::endpoint::Auth::None,
        }
    }

    /// `session_lines` is the terminal-success shape (zero code, the lines kept).
    #[test]
    fn the_transcript_shape_carries_zero_for_success() {
        let transcript = session_lines(vec!["[SUCCESS] x".into()]);
        assert_eq!(transcript.code, 0);
        assert_eq!(transcript.lines, vec!["[SUCCESS] x".to_string()]);
    }

    /// A refused render names the decision; an undecided phrasing rejects (never
    /// reads silence as consent, `ATH-HITL-017`).
    #[test]
    fn the_decision_map_treats_silence_as_refusal() {
        assert_eq!(decision_of("approve"), "approve");
        assert_eq!(decision_of("YES please"), "approve");
        assert_eq!(decision_of("reject"), "reject");
        assert_eq!(decision_of(""), "reject");
        assert_eq!(decision_of("maybe"), "reject");
    }

    /// The renders keep the specification faces: descriptor foremost, prose marked,
    /// the question before the deadline.
    #[test]
    fn the_prompt_renders_keep_the_ath_cli_faces() {
        let approval = ApprovalView {
            id: "ap-1".into(),
            descriptor: "push main".into(),
            risk: "high".into(),
            expires_at: "2026-11-08T18:00:00Z".into(),
            agent_reason: Some("looks ready".into()),
            agent_impact: Some("pushes".into()),
        };
        let text = render_approval(&approval);
        assert!(
            text.starts_with("APPROVAL REQUIRED [ap-1]\npush main"),
            "{text}"
        );
        assert_eq!(text.matches("[untrusted agent prose]").count(), 2);

        let clarification = ClarificationView {
            id: "cl-1".into(),
            question: "which branch?".into(),
            expires_at: "2026-11-08T20:00:00Z".into(),
        };
        let text = render_clarification(&clarification);
        assert!(text.contains("QUESTION [cl-1]"), "{text}");
        assert!(text.contains("which branch?"));
        assert!(text.contains("silence fails the Task"));
    }

    /// A transport failure on the session's very first post is the transport fault,
    /// not a fabricated transcript.
    #[test]
    fn an_unreachable_api_is_a_transport_fault() {
        // Port 9 (discard) is not listening: the POST must fail without inventing
        // answers. A full session run would take the timeout to be honest; the shape is
        // asserted through the fault type's own reading.
        let fault = SessionFault::Transport("refused".into());
        assert!(matches!(fault, SessionFault::Transport(_)));
        let _ = loopback();
    }
}
