//! `ConversationRepository` implemented on `SqliteStore` over §27.3's
//! `conversations` / `utterances` tables (P6-02's supporting layer).

use crate::error::{PersistenceError, Result};
use crate::repository::ConversationRepository;
use athena_types::{Conversation, ConversationId, TaskId, Utterance, UtteranceKind, UtteranceRole};
use rusqlite::{OptionalExtension, params};

fn row_to_conversation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: ConversationId::parse(row.get::<_, String>(0)?)
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?,
        task_id: row.get::<_, Option<String>>(1)?.and_then(TaskId::parse),
        created_at: row.get(2)?,
    })
}

fn row_to_utterance(row: &rusqlite::Row<'_>) -> rusqlite::Result<Utterance> {
    let role_raw: String = row.get(2)?;
    let kind_raw: String = row.get(3)?;
    Ok(Utterance {
        id: athena_types::UtteranceId::parse(row.get::<_, String>(0)?)
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?,
        conversation_id: ConversationId::parse(row.get::<_, String>(1)?)
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?,
        role: UtteranceRole::ALL
            .iter()
            .copied()
            .find(|role| role.as_str() == role_raw)
            .unwrap_or(UtteranceRole::Human),
        kind: UtteranceKind::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == kind_raw)
            .unwrap_or(UtteranceKind::Message),
        body: row.get(4)?,
        clarification_id: row.get(5)?,
        created_at: row.get(6)?,
    })
}

impl ConversationRepository for crate::SqliteStore {
    fn insert(&mut self, conversation: &Conversation) -> Result<()> {
        self.database()
            .connection()
            .execute(
                "INSERT OR IGNORE INTO conversations (id, task_id, created_at) VALUES (?1, ?2, ?3)",
                params![
                    conversation.id.as_str(),
                    conversation.task_id.as_ref().map(|task| task.as_str()),
                    conversation.created_at,
                ],
            )
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        Ok(())
    }

    fn get(&self, id: &ConversationId) -> Result<Option<Conversation>> {
        let face = self
            .database()
            .connection()
            .query_row(
                "SELECT id, task_id, created_at FROM conversations WHERE id = ?1",
                [id.as_str()],
                row_to_conversation,
            )
            .optional()
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        Ok(face)
    }

    fn link_task(&mut self, id: &ConversationId, task: &TaskId) -> Result<()> {
        let touched = self
            .database_mut()
            .connection_mut()
            .execute(
                "UPDATE conversations SET task_id = ?2 WHERE id = ?1",
                params![id.as_str(), task.as_str()],
            )
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        if touched == 0 {
            return Err(PersistenceError::NotFound {
                entity: "conversation",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    fn append_utterance(&mut self, utterance: &Utterance) -> Result<()> {
        self.database()
            .connection()
            .execute(
                "INSERT OR IGNORE INTO utterances (id, conversation_id, role, kind, body, \
                 clarification_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    utterance.id.as_str(),
                    utterance.conversation_id.as_str(),
                    utterance.role.as_str(),
                    utterance.kind.as_str(),
                    utterance.body,
                    utterance.clarification_id,
                    utterance.created_at,
                ],
            )
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        Ok(())
    }

    fn list_utterances(&self, conversation: &ConversationId) -> Result<Vec<Utterance>> {
        let mut statement = self
            .database()
            .connection()
            .prepare(
                "SELECT id, conversation_id, role, kind, body, clarification_id, created_at \
                 FROM utterances WHERE conversation_id = ?1 ORDER BY created_at, id",
            )
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        let rows = statement
            .query_map([conversation.as_str()], row_to_utterance)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<Utterance>>>())
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        Ok(rows)
    }
}

#[cfg(test)]
pub mod conversation_store_tests;
