//! Shared helper for looking up commit-author identity from the `users`
//! table. Used by both `notes::routes` (note CRUD commits) and
//! `settings::store` (settings-note commits).

use sqlx::SqlitePool;

/// Look up (display_name, email) for `user_id` from the `users` table,
/// falling back to sensible placeholders if the row is somehow missing
/// (shouldn't normally happen, since `RequireAuth` implies a session that
/// was created via the OIDC callback, which always upserts a `users` row
/// first).
pub(crate) async fn commit_author(
    db: &SqlitePool,
    user_id: &str,
) -> anyhow::Result<(String, String)> {
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT email, display_name FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(db)
            .await?;

    let (email, display_name) = row.unwrap_or((None, None));
    let name = display_name.unwrap_or_else(|| user_id.to_string());
    let email = email.unwrap_or_else(|| format!("{user_id}@users.rustnote.local"));
    Ok((name, email))
}
