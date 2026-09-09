use horde::template::{self, Plan, Step};
use serde_json::{Value, json};
use std::{collections::BTreeMap, process::Command};

#[test]
fn validate_reports_ignored_names_from_nested_templates_without_retaining_values() {
    let dir = tempfile::tempdir().unwrap();
    let templates = dir.path().join(".horde/templates");
    std::fs::create_dir_all(&templates).unwrap();
    std::fs::write(templates.join("outer.toml"), "name='outer'\nversion='1'\n[[steps]]\nid='child'\ntemplate='inner'\nfuture_parent={private='discard-this-value'}\n").unwrap();
    std::fs::write(templates.join("inner.toml"), "name='inner'\nversion='1'\n[[steps]]\nid='measure'\nkind='command'\ncommand=['true']\nfuture_flag=true\nfuture_options={private='discard-this-value'}\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["validate", "outer", "--repo"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for name in ["future_parent", "future_flag", "future_options"] {
        assert!(stderr.contains(name), "{stderr}");
    }
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let warnings = result["warnings"].to_string();
    for name in ["future_parent", "future_flag", "future_options"] {
        assert!(warnings.contains(name), "{result}");
        assert!(result["steps"][0].get(name).is_none());
    }
    assert!(!String::from_utf8_lossy(&output.stdout).contains("discard-this-value"));
    assert!(!stderr.contains("discard-this-value"));
    let restored: Plan = serde_json::from_value(result).unwrap();
    assert!(restored.steps[0].ignored_fields.is_empty());
    assert_eq!(restored.warnings.len(), 2);
}

#[test]
fn unknown_fields_are_discarded_but_known_types_and_values_still_fail() {
    let steps = template::parse_steps(&json!([{"id":"measure","kind":"command","command":["true"],"future":{"nested":[1,2]},"future_flag":true}])).unwrap();
    assert_eq!(
        steps[0]
            .ignored_fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["future", "future_flag"]
    );
    let saved = serde_json::to_value(&steps).unwrap();
    assert!(saved[0].get("future").is_none());
    let restored: Vec<Step> = serde_json::from_value(saved).unwrap();
    assert!(restored[0].ignored_fields.is_empty());
    for (value, expected) in [
        (
            json!([{"id":"bad","needs":"x","future":true}]),
            "steps[0].needs",
        ),
        (
            json!([{"id":"bad","attempts":"once","future":true}]),
            "steps[0].attempts",
        ),
    ] {
        let error = template::parse_steps(&value).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
    for (value, expected) in [
        (
            json!([{"id":"bad","kind":"commmand","future":true}]),
            "unknown kind commmand",
        ),
        (
            json!([{"id":"a"},{"id":"bad","needs":["a"],"when":{"step":"a","status":"success"},"future":true}]),
            "terminal status (succeeded, failed, or skipped)",
        ),
    ] {
        let steps = template::parse_steps(&value).unwrap();
        let error = template::validate(&steps).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
    let all = template::load_templates(std::path::Path::new("absent")).unwrap();
    let clean = template::compile(
        "simulated",
        &all,
        BTreeMap::from([("task".into(), "example".into())]),
    )
    .unwrap();
    assert!(
        serde_json::to_value(clean)
            .unwrap()
            .get("warnings")
            .is_none()
    );
}
