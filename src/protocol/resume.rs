//! Retry failed work and reconsider only the skipped dependency closure.
use crate::store::Store;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) fn steps(db: &Store, task: &str) -> Result<Vec<Value>> {
    let steps = db.steps(task)?;
    let mut retry: BTreeSet<String> = steps
        .iter()
        .filter(|step| {
            step["state"]
                .as_str()
                .is_some_and(|state| ["failed", "cancelled", "uncertain"].contains(&state))
        })
        .map(|step| Ok(step["name"].as_str().context("step name")?.to_owned()))
        .collect::<Result<_>>()?;
    loop {
        let descendants = steps
            .iter()
            .filter(|step| step["state"] == "skipped")
            .filter_map(|step| {
                let spec = Store::step(step);
                match spec {
                    Ok(spec) if spec.needs.iter().any(|name| retry.contains(name)) => {
                        Some(Ok(spec.id))
                    }
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect::<Result<BTreeSet<_>>>()?;
        let next = retry.union(&descendants).cloned().collect();
        if next == retry {
            break;
        }
        retry = next;
    }
    Ok(steps
        .into_iter()
        .filter(|step| {
            step["name"]
                .as_str()
                .is_some_and(|name| retry.contains(name))
        })
        .collect())
}
