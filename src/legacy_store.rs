//! Preserve the pre-Horde outcome/step schema before normal additive migrations.
use anyhow::{Result, ensure};
use rusqlite::Connection;
use std::path::Path;

fn columns(c: &Connection, table: &str) -> Result<Vec<String>> {
    Ok(c.prepare(&format!("PRAGMA table_info(\"{table}\")"))?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(super) fn migrate(c: &Connection, root: &Path) -> Result<()> {
    let old = columns(c, "tasks")?;
    if !old.iter().any(|v| v == "outcome") || old.iter().any(|v| v == "objective") {
        return Ok(());
    }
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("daemon.lock"))?;
    lock.try_lock_exclusive().map_err(|_| {
        anyhow::anyhow!("stop the legacy Horde daemon before migrating its database")
    })?;
    // Another opener may have completed the migration before this lock.
    if columns(c, "tasks")?.iter().any(|v| v == "objective") {
        return Ok(());
    }
    ensure!(
        columns(c, "outcomes")?.iter().any(|v| v == "objective"),
        "legacy steps exist without their outcomes; retain the database for recovery"
    );
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(
        version == 2 && !columns(c, "outcome_tree")?.is_empty(),
        "legacy database requires schema 2 with its original outcome tree; retain it for manual recovery"
    );
    let missing: i64 = c.query_row(
        "SELECT count(*) FROM outcomes WHERE id NOT IN (SELECT outcome FROM outcome_tree)",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        missing == 0,
        "legacy database has incomplete outcome trees; retain it for manual recovery"
    );
    // The old updater can leave empty new tables behind before failing. Never
    // replace a populated table or merge two independently authoritative trees.
    for table in ["steps", "task_tree", "task_bundles"] {
        if !columns(c, table)?.is_empty() {
            let count: i64 =
                c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            ensure!(
                count == 0,
                "legacy migration found populated {table}; retain the database for recovery"
            );
        }
    }
    for table in ["remote_links", "remote_origins", "remote_context"] {
        if !columns(c, table)?.is_empty() {
            let count: i64 =
                c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            ensure!(
                count == 0,
                "legacy database contains federation records in {table}; preserve it for manual migration before upgrading"
            );
        }
    }
    let backup = root.join(format!("pre-horde-rename-{}.sqlite3", super::id()));
    c.execute("VACUUM INTO ?", [backup.to_string_lossy().as_ref()])?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600))?;
    }
    c.execute_batch("PRAGMA foreign_keys=ON; BEGIN IMMEDIATE;")?;
    let result = (|| -> Result<()> {
        for table in ["steps", "task_tree", "task_bundles"] {
            c.execute_batch(&format!("DROP TABLE IF EXISTS {table}"))?;
        }
        c.execute_batch(
            "ALTER TABLE tasks RENAME TO steps; ALTER TABLE outcomes RENAME TO tasks;",
        )?;
        for (from, to) in [
            ("outcome_tree", "task_tree"),
            ("outcome_bundles", "task_bundles"),
        ] {
            if !columns(c, from)?.is_empty() {
                c.execute_batch(&format!("ALTER TABLE {from} RENAME TO {to}"))?;
            }
        }
        for table in ["attempts", "workers", "artifact_links", "knowledge"] {
            if columns(c, table)?.iter().any(|v| v == "task") {
                c.execute_batch(&format!("ALTER TABLE {table} RENAME COLUMN task TO step"))?;
            }
        }
        for table in [
            "steps",
            "revisions",
            "workers",
            "channels",
            "messages",
            "claims",
            "events",
            "artifact_links",
            "knowledge",
            "questions",
            "integrations",
            "workflow_outputs",
            "external_ops",
            "task_tree",
            "event_receipts",
            "task_bundles",
            "remote_environment_leases",
            "app_environments",
            "local_child_bases",
            "remote_context",
            "remote_links",
            "remote_origins",
        ] {
            if columns(c, table)?.iter().any(|v| v == "outcome") {
                c.execute_batch(&format!(
                    "ALTER TABLE {table} RENAME COLUMN outcome TO task"
                ))?;
            }
        }
        if columns(c, "remote_origins")?
            .iter()
            .any(|v| v == "owner_outcome")
        {
            c.execute_batch(
                "ALTER TABLE remote_origins RENAME COLUMN owner_outcome TO owner_task",
            )?;
        }
        // Historical pinned provider settings and remote packets retain their
        // original bytes. Do not automatically replay work under new contracts.
        c.execute("UPDATE tasks SET status='blocked' WHERE status NOT IN ('succeeded','failed','cancelled')", [])?;
        let broken = c.prepare("PRAGMA foreign_key_check")?.exists([])?;
        ensure!(
            !broken,
            "legacy migration found broken references; original database retained"
        );
        c.execute_batch("COMMIT")?;
        Ok(())
    })();
    if result.is_err() {
        c.execute_batch("ROLLBACK")?;
    }
    result
}
