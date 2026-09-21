use super::*;
#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn local_id_is_rejected_before_queuing() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = Store::open(temp.path())?;
        assert!(
            dispatch(
                &db,
                "runtime_create",
                &json!({"id":"local","profile":"any","request_id":"create"})
            )
            .unwrap_err()
            .to_string()
            .contains("reserved")
        );
        assert!(db.rows("SELECT * FROM runtime_operations", &[])?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn interrupted_rollout_pauses_queued_updates_across_recovery() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = Store::open(temp.path())?;
        for (id, state) in [("first", "running"), ("next", "pending")] {
            db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES(?,'worker','runtime_update','{}',?,0)", params![id,state])?;
        }
        recover_operations(&db)?;
        recover_operations(&db)?;
        tick(&db).await?;
        assert_eq!(
            management::value(&db, "fleet_updates_paused")?.as_deref(),
            Some("true")
        );
        let state: String = db.conn.query_row(
            "SELECT state FROM runtime_operations WHERE id='next'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(state, "pending");
        let state: String = db.conn.query_row(
            "SELECT state FROM runtime_operations WHERE id='first'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(state, "uncertain");
        Ok(())
    }

    #[tokio::test]
    async fn incompatible_container_is_held_without_provider_rollback() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = Store::open(temp.path())?;
        let p: Profile =
            serde_json::from_value(json!({"provider":"docker","image":"old@sha256:fixture"}))?;
        for rollback_safe in [json!(false), Value::Null] {
            let progress = json!({"image":"new@sha256:fixture","phase":"health","replaced_at":0,"rollback_safe":rollback_safe});
            let op = json!({"id":"update","result":progress.to_string()});
            let config = crate::network::NetworkConfig::default();
            let result = container_update(
                &db,
                &p,
                "worker",
                &json!({"resource":"worker"}),
                &op,
                &config,
                "missing",
                &json!({"version":"0.2.1"}),
            )
            .await?;
            assert_eq!(result["state"], "blocked");
            assert!(
                result["reason"]
                    .as_str()
                    .unwrap()
                    .contains("schema compatibility")
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod branding_tests {
    use super::*;
    #[test]
    fn bootstrap_supports_both_generations_of_remote_images() {
        let packet = json!({"fixture": "bootstrap"});
        let env = bootstrap_env(&Profile::default(), Some(&packet));
        assert_eq!(env["HORDE_CONCURRENCY"], "4");
        assert_eq!(env["HORDE_BOOTSTRAP_JSON"], packet.to_string());
    }
}

#[test]
fn bootstrap_packets_are_private_immutable_and_outside_operation_records() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let db = Store::open(temp.path())?;
    let packet = json!({"key":"private-test-material","project":{"id":"default"}});
    project_host::persist_bootstrap(&db, "default", "worker", &packet)?;
    project_host::persist_bootstrap(&db, "default", "worker", &packet)?;
    assert!(
        project_host::persist_bootstrap(&db, "default", "worker", &json!({"key":"changed"}))
            .is_err()
    );
    let path = project_host::bootstrap_path(&db, "default", "worker")?;
    assert_eq!(std::fs::metadata(path)?.permissions().mode() & 0o077, 0);
    assert!(db.rows("SELECT * FROM runtime_operations", &[])?.is_empty());
    Ok(())
}

#[test]
fn remote_reconcile_recovers_exact_resource_and_observed_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let db = Store::open(temp.path())?;
    db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('worker','test','{}','requested',0)",[])?;
    for state in ["stopped", "uncertain", "provisioned"] {
        project_host::record_host_result(
            &db,
            "worker",
            "runtime_reconcile",
            &json!({"guest":{"name":"horde-worker"},"state":state}),
        )?;
        let row = db.rows(
            "SELECT resource,state FROM managed_runtimes WHERE id='worker'",
            &[],
        )?;
        assert_eq!(row[0]["resource"], "horde-worker");
        assert_eq!(row[0]["state"], state);
    }
    assert!(
        project_host::record_host_result(
            &db,
            "worker",
            "runtime_reconcile",
            &json!({"guest":{"name":"foreign-resource"},"state":"stopped"})
        )
        .is_err()
    );
    assert_eq!(
        db.rows("SELECT state FROM managed_runtimes", &[])?[0]["state"],
        "provisioned"
    );
    Ok(())
}
