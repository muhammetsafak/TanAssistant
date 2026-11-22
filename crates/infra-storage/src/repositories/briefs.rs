//! `ActionBriefRepository` over SQLite.

use rusqlite::{params, OptionalExtension};
use tan_application::ports::ActionBriefRepository;
use tan_application::RepositoryError;
use tan_domain::{ActionBrief, ActionItemId, EpochMs, ProviderProfileId};

use super::parse_column;
use crate::db::Database;

/// SQLite-backed [`ActionBriefRepository`].
#[derive(Debug, Clone)]
pub struct SqliteActionBriefRepository {
    db: Database,
}

impl SqliteActionBriefRepository {
    /// Creates the repository over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl ActionBriefRepository for SqliteActionBriefRepository {
    async fn get(
        &self,
        action_item_id: &ActionItemId,
    ) -> Result<Option<ActionBrief>, RepositoryError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT action_item_id, language, content_json, provider_profile_id, created_at
                     FROM action_briefs WHERE action_item_id = ?1",
                    params![action_item_id.as_str()],
                    |row| {
                        Ok(ActionBrief {
                            action_item_id: ActionItemId::new(row.get::<_, String>(0)?),
                            language: parse_column(row, 1)?,
                            content_json: row.get(2)?,
                            provider_profile_id: row
                                .get::<_, Option<String>>(3)?
                                .map(ProviderProfileId::new),
                            created_at: EpochMs::new(row.get(4)?),
                        })
                    },
                )
                .optional()?)
        })
    }

    async fn save(&self, brief: &ActionBrief) -> Result<(), RepositoryError> {
        self.db.with_conn(|conn| {
            // A dangling action item or profile violates a foreign key, which the connection
            // reports as a conflict.
            conn.execute(
                "INSERT INTO action_briefs
                     (action_item_id, language, content_json, provider_profile_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (action_item_id) DO UPDATE SET
                     language = excluded.language,
                     content_json = excluded.content_json,
                     provider_profile_id = excluded.provider_profile_id,
                     created_at = excluded.created_at",
                params![
                    brief.action_item_id.as_str(),
                    brief.language.as_str(),
                    brief.content_json,
                    brief
                        .provider_profile_id
                        .as_ref()
                        .map(ProviderProfileId::as_str),
                    brief.created_at.get(),
                ],
            )?;
            Ok(())
        })
    }
}
