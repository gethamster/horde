use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[test]
fn verifies_a_real_isolated_simulated_task() {
    let report = horde::init_smoke::verify(Path::new(env!("CARGO_BIN_EXE_horde"))).unwrap();
    assert_eq!(report["status"], "passed");
    assert_eq!(report["steps"], 4);
    assert_eq!(report["scope"], "scheduler_and_task_completion");
    assert_eq!(report["provider_calls"], 0);
    assert!(report.get("task").is_none());
    assert!(report.get("data_dir").is_none());
}

#[test]
fn verifies_a_binary_without_adjacent_or_source_checkout_skills() {
    let fixture = tempfile::tempdir().unwrap();
    let binary = fixture.path().join("horde");
    std::fs::copy(env!("CARGO_BIN_EXE_horde"), &binary).unwrap();
    assert!(!fixture.path().join("skills").exists());
    let report = horde::init_smoke::verify(&binary).unwrap();
    assert_eq!(report["status"], "passed");
    assert_eq!(report["steps"], 4);
    assert_eq!(report["provider_calls"], 0);
}

#[test]
fn reports_daemon_startup_failure() {
    let error = horde::init_smoke::verify(Path::new("/usr/bin/false")).unwrap_err();
    assert!(
        error.to_string().contains("smoke daemon exited"),
        "{error:#}"
    );
}

#[test]
fn startup_failure_removes_temporary_state_and_strips_parent_environment() {
    let fixture = tempfile::tempdir().unwrap();
    let binary = fixture.path().join("capture");
    let captured = fixture.path().join("environment");
    let quoted = captured.display().to_string().replace('\'', "'\\''");
    std::fs::write(
        &binary,
        format!("#!/bin/sh\n/usr/bin/env > '{quoted}'\n[ -f \"$2/skill-packs/CURRENT\" ] && printf '\\nSMOKE_PACK_READY=yes\\n' >> '{quoted}'\nexit 1\n"),
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    horde::init_smoke::verify(&binary).unwrap_err();
    let environment = std::fs::read_to_string(captured).unwrap();
    let home = environment
        .lines()
        .find_map(|line| line.strip_prefix("HOME="))
        .unwrap();
    assert!(!Path::new(home).parent().unwrap().exists());
    assert!(!environment.lines().any(|line| line.starts_with("HORDE_")
        || line.starts_with("TUARA_")
        || line.starts_with("OPENAI_")
        || line.starts_with("ANTHROPIC_")));
    assert!(environment.lines().any(|line| line.starts_with("PATH=")));
    assert!(
        environment
            .lines()
            .any(|line| line == "SMOKE_PACK_READY=yes")
    );
}
