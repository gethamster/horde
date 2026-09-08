use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
};
fn executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
#[test]
fn remote_install_preserves_bootstrap_stdin_and_reuses_existing_install() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    executable(
        &bin.join("curl"),
        r#"#!/bin/sh
set -eu
while [ "$1" != '-o' ]; do shift; done
shift
cat > "$1" <<'INSTALL'
set -eu
# The installer must not be able to consume the credential packet.
test -z "$(cat)"
printf 'installed\n' >> "$HOME/install-count"
cat > "$HOME/.local/bin/horde" <<'TASK'
#!/bin/sh
printf '%s\n' "$*" > "$HOME/args"
cat > "$HOME/received"
TASK
chmod 700 "$HOME/.local/bin/horde"
INSTALL
"#,
    );
    for service in [false, true] {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(horde::pairing::remote_script(service))
            .env("HOME", temp.path())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"private-bootstrap-packet")
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("received")).unwrap(),
            "private-bootstrap-packet"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("args")).unwrap(),
            if service {
                "network accept --service\n"
            } else {
                "network accept\n"
            }
        );
    }
    assert_eq!(
        std::fs::read_to_string(temp.path().join("install-count")).unwrap(),
        "installed\n"
    );
}
#[test]
fn workers_cannot_invoke_setup_or_accept() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["network", "setup"],
        vec!["network", "accept"],
        vec!["network", "add", "alice@worker"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(temp.path())
            .args(args)
            .env("HORDE_WORKER_TOKEN", "scoped-worker")
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("administrative access"));
    }
}

#[test]
fn pairing_reuses_legacy_worker_without_consuming_bootstrap() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(".local/bin")).unwrap();
    executable(
        &temp.path().join(".local/bin/task"),
        "#!/bin/sh\nprintf '%s' \"$*\" > \"$HOME/args\"\ncat > \"$HOME/received\"\n",
    );
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(horde::pairing::remote_script(true))
        .env("HOME", temp.path())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"bootstrap-fixture")
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("args")).unwrap(),
        "network accept --service"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("received")).unwrap(),
        "bootstrap-fixture"
    );
}
