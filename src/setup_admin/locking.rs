use super::{Reply, error};
use axum::http::StatusCode;
use fs2::FileExt;
use std::{fs::File, path::Path};

pub(super) fn with_lock(root: &Path, operation: impl FnOnce(&File) -> Reply) -> Reply {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("setup-admin.lock"))
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "setup lock unavailable"))?;
    lock.try_lock_exclusive().map_err(|_| {
        error(
            StatusCode::TOO_MANY_REQUESTS,
            "another setup operation is active",
        )
    })?;
    let result = operation(&lock);
    // Closing only our descriptor can leave the lock in a concurrently forked
    // child. Release the shared file description on every normal return path.
    FileExt::unlock(&lock).map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "setup lock release failed",
        )
    })?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use serde_json::json;

    #[test]
    fn releases_shared_description_after_success_and_early_failure() {
        for failed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut inherited = None;
            let result = with_lock(dir.path(), |lock| {
                // A forked process temporarily owns this same file description.
                inherited = Some(lock.try_clone().unwrap());
                if failed {
                    Err(error(StatusCode::CONFLICT, "fixture conflict"))
                } else {
                    Ok(Json(json!({"state":"succeeded"})))
                }
            });
            assert_eq!(result.is_err(), failed);
            let contender = std::fs::OpenOptions::new()
                .write(true)
                .open(dir.path().join("setup-admin.lock"))
                .unwrap();
            contender
                .try_lock_exclusive()
                .expect("completed setup retains its lock in an inherited descriptor");
            FileExt::unlock(&contender).unwrap();
            drop(inherited);
        }
    }
}
