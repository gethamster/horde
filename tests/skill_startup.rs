use horde::{skill_catalog, skills};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf, process::Command};

struct Fixture {
    _dir: tempfile::TempDir,
    binary: PathBuf,
    root: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("horde");
        std::fs::copy(env!("CARGO_BIN_EXE_horde"), &binary).unwrap();
        let root = dir.path().join("data");
        std::fs::create_dir(&root).unwrap();
        let config = dir.path().join("config");
        Self {
            _dir: dir,
            binary,
            root,
            config,
        }
    }

    fn command(&self, name: &str) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .arg("--data-dir")
            .arg(&self.root)
            .arg(name)
            .env("XDG_CONFIG_HOME", &self.config)
            .env_remove("HORDE_WORKER_TOKEN")
            .env_remove("HORDE_ENROLLMENT_FILE")
            .env_remove("HORDE_ENROLLMENT_JSON")
            .current_dir(self.binary.parent().unwrap());
        command
    }

    fn pack(&self) -> PathBuf {
        let path = self.binary.parent().unwrap().join("skills");
        std::fs::create_dir_all(path.join("fixture")).unwrap();
        std::fs::write(path.join("fixture/SKILL.md"), "Read only when selected.").unwrap();
        path
    }
}

#[test]
fn copied_source_binary_reports_missing_pack_before_start_or_doctor_probe() {
    let fixture = Fixture::new();
    for args in [
        vec!["start"],
        vec!["doctor"],
        vec!["doctor", "--probe", "--provider", "missing"],
    ] {
        let output = fixture.command(args[0]).args(&args[1..]).output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("no skill pack installed"), "{error}");
        assert!(
            error.contains(
                fixture
                    .binary
                    .parent()
                    .unwrap()
                    .join("skills")
                    .to_str()
                    .unwrap()
            ),
            "{error}"
        );
        assert!(
            error.contains(fixture.root.join("skill-packs/CURRENT").to_str().unwrap()),
            "{error}"
        );
        assert!(!fixture.root.join("daemon.sock").exists());
    }
    // Template validation is deliberately independent of runtime readiness.
    assert!(
        fixture
            .command("validate")
            .arg("simulated")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn doctor_and_submission_resolve_the_same_adjacent_or_installed_pack() {
    let fixture = Fixture::new();
    let adjacent = fixture.pack();
    let output = fixture.command("doctor").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let packet = skill_catalog::load_from(&adjacent).unwrap();
    assert_eq!(
        report["skill_pack"]["hash"],
        skill_catalog::report(&packet).unwrap()["hash"]
    );
    assert_eq!(report["skill_pack"]["path"], adjacent.to_str().unwrap());
    assert_eq!(
        report["skill_pack"]["skills"],
        serde_json::json!(["fixture"])
    );

    let installed = skills::capture(
        &adjacent,
        &BTreeMap::from([("installed".into(), adjacent.join("fixture"))]),
    )
    .unwrap();
    let installed_report = skill_catalog::install(&fixture.root, &installed).unwrap();
    std::fs::remove_dir_all(&adjacent).unwrap();
    let output = fixture.command("doctor").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["skill_pack"]["hash"], installed_report["hash"]);
    assert_eq!(
        skill_catalog::report(&skill_catalog::load_for(&fixture.root).unwrap()).unwrap()["hash"],
        installed_report["hash"]
    );
    assert!(
        report["skill_pack"]["path"]
            .as_str()
            .unwrap()
            .contains("skill-packs/versions/")
    );
}

#[test]
fn invalid_installed_pack_is_reported_without_using_valid_adjacent_files() {
    let fixture = Fixture::new();
    fixture.pack();
    std::fs::create_dir_all(fixture.root.join("skill-packs/versions")).unwrap();
    let current = fixture.root.join("skill-packs/CURRENT");
    std::fs::write(&current, "invalid").unwrap();
    for name in ["start", "doctor"] {
        let output = fixture.command(name).output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("invalid current skill pack hash"), "{error}");
        assert!(error.contains(current.to_str().unwrap()), "{error}");
    }
    assert_eq!(std::fs::read_to_string(current).unwrap(), "invalid");
    assert!(!fixture.root.join("daemon.sock").exists());
}
