//! Frozen admission intent and terminal receipts preserve lost-response effects.
use super::*;

pub(crate) fn pending(db: &Store) -> Result<i64> {
    Ok(db.conn.query_row(
        "SELECT COUNT(*) FROM preview_jobs j WHERE j.reservation IS NOT NULL OR EXISTS(SELECT 1 FROM preview_admissions a LEFT JOIN preview_admission_receipts r ON r.job=a.job AND r.ordinal=a.ordinal WHERE a.job=j.id AND (r.state IS NULL OR r.state NOT IN ('held','released','expired')))",
        [], |row|row.get(0),
    )?)
}

pub(super) fn intent(db: &Store, id: &str, ordinal: i64, request: &Value) -> Result<String> {
    let key = request["idempotency_key"]
        .as_str()
        .context("admission key")?;
    let scope = request["scope"].as_str().context("admission scope")?;
    let rid = crate::store::hash(format!("{scope}:{key}").as_bytes());
    db.atomic(|| {
        db.conn.execute("INSERT OR IGNORE INTO preview_admissions VALUES(?,?,?)",params![id,ordinal,key])?;
        let saved_key:String=db.conn.query_row("SELECT key FROM preview_admissions WHERE job=? AND ordinal=?",params![id,ordinal],|r|r.get(0))?;
        ensure!(saved_key==key,"admission ordinal key changed");
        let prior=db.rows("SELECT request,reservation FROM preview_admission_receipts WHERE job=? AND ordinal=?",&[&id,&ordinal])?;
        if let Some(prior)=prior.first(){ensure!(prior["reservation"]==rid,"admission reservation changed under existing key");}
        if let Some(prior)=prior.first().and_then(|r|r["request"].as_str()) {
            ensure!(serde_json::from_str::<Value>(prior)?==*request,"admission intent changed under existing key");
        }
        db.conn.execute("INSERT INTO preview_admission_receipts VALUES(?,?,?,?,'pending',1) ON CONFLICT(job,ordinal) DO UPDATE SET request=excluded.request,state='pending',generation=generation+1",params![id,ordinal,rid,request.to_string()])?;
        db.conn.execute("UPDATE preview_jobs SET reservation=? WHERE id=?",params![rid,id])?;
        Ok(rid.clone())
    })
}

pub(super) fn recorded(db: &Store, job: &Value, scope: &str) -> Result<Option<Value>> {
    let id = job["id"].as_str().context("preview job")?;
    let candidates=db.rows("SELECT a.ordinal,a.key,r.reservation,r.request,r.state,r.generation FROM preview_admissions a LEFT JOIN preview_admission_receipts r ON r.job=a.job AND r.ordinal=a.ordinal WHERE a.job=? AND (r.state IS NULL OR r.state NOT IN ('held','released','expired')) ORDER BY a.ordinal LIMIT 1",&[&id])?;
    let Some(candidate) = candidates.first() else {
        return Ok(None);
    };
    if !candidate["state"].is_null() {
        return Ok(Some(candidate.clone()));
    }
    let ordinal = candidate["ordinal"].as_i64().context("admission ordinal")?;
    let key = candidate["key"].as_str().context("admission key")?;
    let max: i64 = db.conn.query_row(
        "SELECT MAX(ordinal) FROM preview_admissions WHERE job=?",
        [id],
        |r| r.get(0),
    )?;
    let rid = if ordinal == max {
        job["reservation"].as_str().map(str::to_owned)
    } else {
        None
    }
    .unwrap_or_else(|| crate::store::hash(format!("{scope}:{key}").as_bytes()));
    db.conn.execute(
        "INSERT OR IGNORE INTO preview_admission_receipts VALUES(?,?,?,NULL,'pending',0)",
        params![id, ordinal, rid],
    )?;
    let mut recorded = candidate.clone();
    recorded["reservation"] = json!(rid);
    recorded["state"] = json!("pending");
    recorded["generation"] = json!(0);
    Ok(Some(recorded))
}

pub(super) fn settle(
    db: &Store,
    job: &Value,
    record: &Value,
    state: &str,
    request: &Value,
) -> Result<bool> {
    let id = job["id"].as_str().context("preview job")?;
    let ordinal = record["ordinal"].as_i64().context("admission ordinal")?;
    let rid = record["reservation"]
        .as_str()
        .context("admission reservation")?;
    let generation = record["generation"]
        .as_i64()
        .context("admission generation")?;
    db.atomic(|| {
        let changed=db.conn.execute("UPDATE preview_admission_receipts SET state=?,request=COALESCE(request,?) WHERE job=? AND ordinal=? AND reservation=? AND generation=?",params![state,request.to_string(),id,ordinal,rid,generation])?;
        if changed==0{return Ok(false);}
        let terminal=matches!(state,"held"|"released"|"expired");
        if terminal {
            db.conn.execute("UPDATE preview_jobs SET reservation=NULL WHERE id=? AND reservation=?",params![id,rid])?;
            db.event(job["task"].as_str().context("preview task")?,"run.preview_reservation_reconciled",json!({"id":id,"ordinal":ordinal,"state":state}))?;
        }
        Ok(terminal)
    })
}

pub(super) fn release(db: &Store, id: &str, rid: &str, state: &str) -> Result<()> {
    db.atomic(|| {
        db.conn.execute(
            "UPDATE preview_admission_receipts SET state=? WHERE job=? AND reservation=?",
            params![state, id, rid],
        )?;
        db.conn.execute(
            "UPDATE preview_jobs SET reservation=NULL WHERE id=? AND reservation=?",
            params![id, rid],
        )?;
        Ok(())
    })
}
