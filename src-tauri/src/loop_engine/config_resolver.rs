//! Resolve an issue's effective Loop Contract config. An issue either stores its
//! own `config` JSON, or leaves it `NULL` to inherit the space's
//! `default_config`, resolved at read time so a space-default change propagates
//! to every inheriting issue without rewriting their rows.

use sea_orm::{DatabaseConnection, EntityTrait};

use crate::db::entities::{loop_issue, loop_space};
use crate::loop_engine::error::LoopError;
use crate::models::loops::IssueConfig;

/// The config the engine should act on for `issue`. An issue with its own
/// `config` parses that; an issue with `config = NULL` resolves the space
/// `default_config` (always present). Malformed JSON is a hard error
/// ([`LoopError::InvalidConfig`]) — the engine never silently downgrades a broken
/// config to the default.
pub async fn effective_config(
    conn: &DatabaseConnection,
    issue: &loop_issue::Model,
) -> Result<IssueConfig, LoopError> {
    match issue.config.as_deref() {
        Some(json) => {
            serde_json::from_str(json).map_err(|e| LoopError::InvalidConfig(e.to_string()))
        }
        None => {
            let space = loop_space::Entity::find_by_id(issue.space_id)
                .one(conn)
                .await?
                .ok_or_else(|| LoopError::NotFound(format!("loop_space {}", issue.space_id)))?;
            serde_json::from_str(&space.default_config)
                .map_err(|e| LoopError::InvalidConfig(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::service::loop_service::{issue, space};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, QueryFilter};

    async fn fetch_issue(db: &crate::db::AppDatabase, id: i32) -> loop_issue::Model {
        loop_issue::Entity::find_by_id(id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
    }

    /// Create a space + an inheriting issue (`config = NULL`); returns
    /// (db, space_id, issue_id).
    async fn seed() -> (crate::db::AppDatabase, i32, i32) {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/cfg-resolver").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let detail = issue::create_issue(
            &db.conn,
            space.id,
            "Issue",
            "body",
            IssuePriority::Medium,
            None, // inheriting
        )
        .await
        .unwrap();
        (db, space.id, detail.row.id)
    }

    /// Overwrite the space's `default_config` (NOT NULL) with the given JSON.
    async fn set_space_default(db: &crate::db::AppDatabase, space_id: i32, json: String) {
        loop_space::Entity::update_many()
            .col_expr(loop_space::Column::DefaultConfig, Expr::value(json))
            .filter(loop_space::Column::Id.eq(space_id))
            .exec(&db.conn)
            .await
            .unwrap();
    }

    /// Overwrite an issue's `config` (nullable: `None` = inherit).
    async fn set_issue_config(db: &crate::db::AppDatabase, issue_id: i32, json: Option<String>) {
        loop_issue::Entity::update_many()
            .col_expr(loop_issue::Column::Config, Expr::value(json))
            .filter(loop_issue::Column::Id.eq(issue_id))
            .exec(&db.conn)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn inheriting_issue_resolves_space_default() {
        let (db, space_id, issue_id) = seed().await;
        let space_default = IssueConfig {
            max_attempts: 99,
            ..IssueConfig::default()
        };
        set_space_default(&db, space_id, serde_json::to_string(&space_default).unwrap()).await;

        let cfg = effective_config(&db.conn, &fetch_issue(&db, issue_id).await)
            .await
            .unwrap();
        assert_eq!(cfg.max_attempts, 99, "inherits the space default");
    }

    #[tokio::test]
    async fn fresh_space_default_is_the_engine_default() {
        // A freshly created space stores the engine default, so an inheriting
        // issue resolves it without any explicit set.
        let (db, _space_id, issue_id) = seed().await;
        let cfg = effective_config(&db.conn, &fetch_issue(&db, issue_id).await)
            .await
            .unwrap();
        assert_eq!(cfg.max_attempts, IssueConfig::default().max_attempts);
    }

    #[tokio::test]
    async fn custom_issue_uses_its_own_config() {
        let (db, space_id, issue_id) = seed().await;
        // A space default exists, but the issue has its own config → ignored.
        set_space_default(
            &db,
            space_id,
            serde_json::to_string(&IssueConfig {
                max_attempts: 99,
                ..IssueConfig::default()
            })
            .unwrap(),
        )
        .await;
        let own = IssueConfig {
            max_attempts: 42,
            ..IssueConfig::default()
        };
        set_issue_config(&db, issue_id, Some(serde_json::to_string(&own).unwrap())).await;

        let cfg = effective_config(&db.conn, &fetch_issue(&db, issue_id).await)
            .await
            .unwrap();
        assert_eq!(cfg.max_attempts, 42, "uses its own config, not the space default");
    }

    #[tokio::test]
    async fn malformed_config_is_hard_error() {
        let (db, _space_id, issue_id) = seed().await;
        set_issue_config(&db, issue_id, Some("{not valid json".to_string())).await;
        let err = effective_config(&db.conn, &fetch_issue(&db, issue_id).await)
            .await
            .unwrap_err();
        assert!(matches!(err, LoopError::InvalidConfig(_)));
    }
}
