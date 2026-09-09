use super::*;

#[test]
fn join_accepts_file_and_legacy_flag_but_not_both() {
    for args in [
        vec![
            "horde",
            "network",
            "join",
            "workers.json",
            "--name",
            "apollo",
        ],
        vec![
            "horde",
            "network",
            "join",
            "--invitation",
            "workers.json",
            "--no-start",
        ],
    ] {
        assert!(Cli::try_parse_from(args).is_ok());
    }
    assert!(Cli::try_parse_from(["horde", "network", "join"]).is_err());
    assert!(
        Cli::try_parse_from([
            "horde",
            "network",
            "join",
            "first.json",
            "--invitation",
            "second.json"
        ])
        .is_err()
    );
}

#[test]
fn submit_can_target_a_named_runtime_without_a_parent() {
    let cli = Cli::try_parse_from(["horde", "submit", "--on", "apollo", "Fix the tests"]).unwrap();
    assert!(matches!(cli.command, Commands::Submit { on: Some(name), .. } if name=="apollo"));
}

#[test]
fn runtime_management_does_not_require_raw_json_or_request_ids() {
    for args in [
        vec!["horde", "runtime", "rename", "worker-id", "apollo"],
        vec!["horde", "runtime", "remove", "apollo"],
        vec!["horde", "runtime", "list", "--json"],
    ] {
        assert!(Cli::try_parse_from(args).is_ok());
    }
}

#[test]
fn worker_updates_choose_binary_or_skills_without_manual_request_ids() {
    for args in [
        vec!["horde", "runtime", "update", "apollo", "--skills"],
        vec!["horde", "runtime", "update", "apollo", "--version", "0.7.0"],
        vec!["horde", "skills", "install", "./skills"],
        vec!["horde", "skills", "list"],
    ] {
        assert!(Cli::try_parse_from(args.clone()).is_ok(), "{args:?}");
    }
    for args in [
        vec!["horde", "runtime", "update", "apollo"],
        vec![
            "horde",
            "runtime",
            "update",
            "apollo",
            "--skills",
            "--version",
            "0.7.0",
        ],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}
