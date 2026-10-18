//! The `athena chat` interaction layer (P6-01; `ATH-CLI-003` / `004` / `005`):
//! the conversation face's three renders — the progress display (`ATH-CLI-005`), the
//! in-conversation approval presentation (`ATH-CLI-004`), and the clarification
//! answering. The HTTP transport arrives with P6-02/03; this layer is the rendering +
//! decision contract each chat leg resolves against (deterministic, Core-API-only —
//! `ATH-CLI-001`).

use serde::Deserialize;

/// The Task's state as the chat face reads it (a local wire-form copy — `athena-cli`
/// rides the Core API only, §32.2: no `athena-types` edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChatTaskState {
    /// Created.
    Created,
    /// Queued.
    Queued,
    /// Running.
    Running,
    /// Suspended (WAITING_APPROVAL / WAITING_INPUT / RETRYING / REPLANNING / EVALUATING).
    Suspended,
    /// Success.
    Success,
    /// Failed.
    Failed,
    /// Cancelled.
    Cancelled,
}

impl ChatTaskState {
    /// The rendered token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "CREATED",
            Self::Queued => "QUEUED",
            Self::Running => "RUNNING",
            Self::Suspended => "SUSPENDED",
            Self::Success => "SUCCESS",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        }
    }
    /// Whether the state is terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Success | Self::Failed | Self::Cancelled)
    }
}

/// The progress facts the chat face renders (`ATH-CLI-005`: 現在の PlanStep, 実行中 Agent).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Progress {
    /// The owning Task.
    pub task_id: String,
    /// The Task's state.
    pub task_state: ChatTaskState,
    /// The current PlanStep's intent (`None` when no step runs).
    pub current_step: Option<String>,
    /// The running agents' names, in assignment order.
    pub running_agents: Vec<String>,
}

impl Progress {
    /// The rendered progress block the chat shows after each event batch.
    #[must_use]
    pub fn rendered(&self) -> String {
        let mut text = format!("[{}] task {}", self.task_state.as_str(), self.task_id);
        if let Some(step) = &self.current_step {
            text.push_str(&format!(" — now: {step}"));
        }
        if !self.running_agents.is_empty() {
            text.push_str(&format!(" — running: {}", self.running_agents.join(", ")));
        }
        text
    }
}

/// The approval request the chat face presents (`ATH-CLI-004`): the **canonical
/// descriptor foremost** (`ATH-HITL-010`), and the Agent's `reason` / `impact` prose
/// always marked **untrusted** (`ATH-HITL-011`).
#[derive(Default, Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ApprovalPrompt {
    /// The canonical descriptor the Core Host built itself (`ATH-HITL-010`).
    pub canonical_descriptor: String,
    /// The risk, from configuration (`ATH-CORE-038` — never the Agent's prose).
    pub risk_level: String,
    /// The `expires_at` (`timeout.approval`).
    pub expires_at: String,
    /// The Agent's `reason` prose — untrusted, shown demoted.
    pub agent_reason: Option<String>,
    /// The Agent's `impact` prose — untrusted, shown demoted.
    pub agent_impact: Option<String>,
    /// The pending approval's identifier.
    pub approval_id: String,
}

impl ApprovalPrompt {
    /// The in-chat presentation: the descriptor first; the Agent's prose demoted under
    /// a visible untrusted marker (the human never has to trust the agent's phrasing
    /// to decide, `ATH-HITL-011`).
    #[must_use]
    pub fn rendered(&self) -> String {
        let mut text = format!(
            "APPROVAL REQUIRED [{}]\n{}\nrisk: {} • expires: {}\n",
            self.approval_id, self.canonical_descriptor, self.risk_level, self.expires_at
        );
        if let Some(reason) = &self.agent_reason {
            text.push_str(&format!("  [untrusted agent prose] reason: {reason}\n"));
        }
        if let Some(impact) = &self.agent_impact {
            text.push_str(&format!("  [untrusted agent prose] impact: {impact}\n"));
        }
        text.push_str("reply `approve` or `reject` in this conversation.\n");
        text
    }
}

/// The clarification the chat face answers (`ATH-HITL-020`).
#[derive(Default, Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClarificationPrompt {
    /// The clarification's identifier.
    pub clarification_id: String,
    /// The owning Task (the answering endpoint's path segment).
    pub task_id: String,
    /// The Core Agent's question.
    pub question: String,
    /// The answer deadline (`timeout.clarification`; expiry fails the Task,
    /// `ATH-HITL-017`).
    pub expires_at: String,
}

impl ClarificationPrompt {
    /// The in-chat question presentation.
    #[must_use]
    pub fn rendered(&self) -> String {
        format!(
            "QUESTION [{}]\n{}\n(answer in this conversation; deadline {expires_at} — silence fails the Task)",
            self.clarification_id,
            self.question,
            expires_at = self.expires_at
        )
    }
}

/// Why a chat turn was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatTurnFault {
    /// The pending record the turn asked about is not present.
    #[error("unknown pending record {0:?}")]
    UnknownPending(String),
    /// The answer was silence — silence is never consent (`ATH-HITL-017`).
    #[error("the answer is empty; silence is never consent")]
    EmptyAnswer,
}

/// One chat turn's answer plan: the endpoint + body the transport layer posts
/// (`ATH-HITL-020` — `POST /api/v1/tasks/{id}/answer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerPlan {
    /// The HTTP method.
    pub method: &'static str,
    /// The endpoint path.
    pub path: String,
    /// The JSON body (`clarification_id` + `answer`).
    pub body_json: String,
}

/// Answer one pending clarification in the conversation.
///
/// # Errors
/// [`ChatTurnFault::UnknownPending`] when the prompt carries no question,
/// [`ChatTurnFault::EmptyAnswer`] when the answer is silence.
pub fn answer_plan(
    prompt: &ClarificationPrompt,
    answer: &str,
) -> Result<AnswerPlan, ChatTurnFault> {
    if prompt.question.trim().is_empty() {
        return Err(ChatTurnFault::UnknownPending(
            prompt.clarification_id.clone(),
        ));
    }
    if answer.trim().is_empty() {
        return Err(ChatTurnFault::EmptyAnswer);
    }
    Ok(AnswerPlan {
        method: "POST",
        path: format!("/api/v1/tasks/{}/answer", prompt.task_id),
        body_json: format!(
            r#"{{"clarification_id":"{}","answer":"{}"}}"#,
            prompt.clarification_id, answer
        ),
    })
}

/// Answer a pending approval in the conversation: the approve/reject post plan.
///
/// # Errors
/// [`ChatTurnFault::EmptyAnswer`] when the phrasing is neither approve nor reject.
pub fn approval_decision_plan(
    prompt: &ApprovalPrompt,
    phrasing: &str,
) -> Result<AnswerPlan, ChatTurnFault> {
    let lowered = phrasing.trim().to_lowercase();
    let decision = if lowered.starts_with("approve") || lowered.starts_with("yes") {
        "approve"
    } else if lowered.starts_with("reject") || lowered.starts_with("no") {
        "reject"
    } else {
        return Err(ChatTurnFault::EmptyAnswer);
    };
    Ok(AnswerPlan {
        method: "POST",
        path: format!("/api/v1/approvals/{}/{}", prompt.approval_id, decision),
        body_json: String::new(),
    })
}

#[cfg(test)]
mod chat_tests;

impl Default for Progress {
    fn default() -> Self {
        Self {
            task_id: String::new(),
            task_state: ChatTaskState::Created,
            current_step: None,
            running_agents: Vec::new(),
        }
    }
}
