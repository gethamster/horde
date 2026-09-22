//! Bounded checks of the Docker daemon and Compose in AX workers.
use std::{
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Support {
    pub docker: bool,
    pub compose: bool,
}

struct Sample {
    observed: Instant,
    support: Support,
}

/// AX runner environments are fixed for the lifetime of the process. Cache the
/// probe so heartbeat publication never repeatedly starts Docker processes.
pub(super) fn support() -> Support {
    static CACHE: OnceLock<Mutex<Option<Sample>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(sample) = cache.as_ref()
        && sample.observed.elapsed() < Duration::from_secs(60)
    {
        return sample.support;
    }
    let support = probe(|args| succeeds("docker", args, Duration::from_secs(2)));
    *cache = Some(Sample {
        observed: Instant::now(),
        support,
    });
    support
}

fn probe(mut run: impl FnMut(&[&str]) -> bool) -> Support {
    let docker = run(&["info", "--format", "{{.ServerVersion}}"]);
    Support {
        docker,
        compose: docker && run(&["compose", "version", "--short"]),
    }
}

fn succeeds(program: &str, args: &[&str], timeout: Duration) -> bool {
    use std::os::unix::process::CommandExt;
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                // The probe owns this process group, including any CLI helpers.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_client_without_daemon_is_not_docker_support() {
        let mut calls = 0;
        let support = probe(|args| {
            calls += 1;
            assert_eq!(args[0], "info");
            false
        });
        assert_eq!(
            support,
            Support {
                docker: false,
                compose: false
            }
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn compose_requires_a_working_daemon_and_plugin() {
        assert_eq!(
            probe(|_| true),
            Support {
                docker: true,
                compose: true
            }
        );
        assert_eq!(
            probe(|args| args[0] == "info"),
            Support {
                docker: true,
                compose: false
            }
        );
    }

    #[test]
    fn command_probe_handles_missing_failure_and_timeout() {
        assert!(!succeeds(
            "/missing/horde-test-docker",
            &[],
            Duration::from_secs(1)
        ));
        assert!(!succeeds(
            "/bin/sh",
            &["-c", "exit 1"],
            Duration::from_secs(1)
        ));
        assert!(succeeds(
            "/bin/sh",
            &["-c", "exit 0"],
            Duration::from_secs(1)
        ));
        let started = Instant::now();
        assert!(!succeeds(
            "/bin/sh",
            &["-c", "sleep 30"],
            Duration::from_millis(50)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
