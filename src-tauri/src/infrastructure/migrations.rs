// Dead-simple forward-only migrations driven by SQLite's PRAGMA user_version.
// Each entry in `MIGRATIONS` is applied in a single transaction; failures roll
// back the whole transaction, so the DB never ends up in a half-migrated state.

use rusqlite::Connection;

use super::db::DbError;

const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/0001_init.sql"),
    include_str!("../../migrations/0002_fts_contentless_delete.sql"),
    include_str!("../../migrations/0003_add_forwarded.sql"),
    include_str!("../../migrations/0004_aliases_and_folders.sql"),
    include_str!("../../migrations/0005_archive_on_reply.sql"),
    include_str!("../../migrations/0006_spam_folder.sql"),
    include_str!("../../migrations/0007_prefetch_days.sql"),
    include_str!("../../migrations/0008_unescape_subjects.sql"),
    include_str!("../../migrations/0009_junk_flag.sql"),
    include_str!("../../migrations/0010_spam_rules.sql"),
    include_str!("../../migrations/0011_unified_starred.sql"),
    include_str!("../../migrations/0012_folder_sync_enabled.sql"),
    include_str!("../../migrations/0013_workflows.sql"),
    include_str!("../../migrations/0014_workflow_archive_after.sql"),
    include_str!("../../migrations/0015_workflow_rules.sql"),
    include_str!("../../migrations/0016_workflow_rule_folder.sql"),
    include_str!("../../migrations/0017_workflow_training_candidates.sql"),
    include_str!("../../migrations/0018_envelope_has_attachments.sql"),
    include_str!("../../migrations/0019_account_sync_mode.sql"),
    include_str!("../../migrations/0020_account_server_stores_sent.sql"),
    include_str!("../../migrations/0021_contacts.sql"),
    include_str!("../../migrations/0022_address_history_fix_incoming.sql"),
    include_str!("../../migrations/0023_contact_tags.sql"),
    include_str!("../../migrations/0024_lifetime_rules.sql"),
    include_str!("../../migrations/0025_unify_filters_into_workflow_rules.sql"),
    include_str!("../../migrations/0026_delay_minutes.sql"),
    include_str!("../../migrations/0027_commitments.sql"),
    include_str!("../../migrations/0028_commitment_status.sql"),
    include_str!("../../migrations/0029_last_published_sequence.sql"),
    include_str!("../../migrations/0030_negotiations.sql"),
];

pub fn apply(conn: &mut Connection) -> Result<(), DbError> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let current = current.max(0) as usize;

    for (idx, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        let next = (idx + 1) as i64;
        tx.pragma_update(None, "user_version", next)?;
        tx.commit()?;
        tracing::info!("db migration {next} applied");
    }

    Ok(())
}
