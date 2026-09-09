use horde::execution_selection;
use serde_json::{Value, json};

fn inventory() -> Value {
    json!({"runtimes":[
        {"runtime":"controller","name":"laptop","local":true,"fresh":true,"ready":true,"capacity":{"available":4,"draining":false},"capabilities":[{"id":"codex","provider":"codex","model":"astra","kind":"codex","available":null,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]},
        {"runtime":"worker-1","name":"apollo","protocol":{"features":["execution_selection"]},"local":false,"fresh":true,"ready":true,"capacity":{"available":4,"draining":false},"capabilities":[{"id":"claude","provider":"claude","model":"opus","kind":"claude","available":null,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},{"id":"codex","provider":"codex","model":"sol","kind":"codex","available":null,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}
    ]})
}

#[test]
fn pins_allowed_pairs_and_rejects_crossed_runtime_capabilities() {
    let scope = json!({"allowed":[{"runtime":"local","capabilities":["codex"]},{"runtime":"apollo","capabilities":["claude"]}],"selected":{"runtime":"local","capability":"codex"}});
    let parent = execution_selection::prepare_inventory(&inventory(), &scope, None).unwrap();
    assert_eq!(parent["selected"]["runtime"], "controller");
    assert_eq!(parent["selected"]["model"], "astra");
    assert!(
        execution_selection::prepare_inventory(
            &inventory(),
            &json!({"selected":{"runtime":"apollo","capability":"codex"}}),
            Some(&parent)
        )
        .is_err()
    );
    let child = execution_selection::prepare_inventory(&inventory(), &json!({"allowed":[{"runtime":"apollo","capabilities":["claude"]}],"selected":{"runtime":"apollo","capability":"claude"}}), Some(&parent)).unwrap();
    assert_eq!(child["selected"]["model"], "opus");
}

#[test]
fn rejects_model_drift_and_stale_selected_runtime() {
    let selection = json!({"allowed":[{"runtime":"apollo","capabilities":["claude"]}],"selected":{"runtime":"apollo","capability":"claude"}});
    let parent = execution_selection::prepare_inventory(&inventory(), &selection, None).unwrap();
    let changed = json!({"runtimes":[{"runtime":"worker-1","name":"apollo","protocol":{"features":["execution_selection"]},"fresh":true,"ready":true,"capacity":{"available":4,"draining":false},"capabilities":[{"id":"claude","provider":"claude","model":"different-model","kind":"claude","available":null,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}]});
    assert!(
        execution_selection::prepare_inventory(&changed, &selection, Some(&parent))
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    let stale = json!({"runtimes":[{"runtime":"worker-1","name":"apollo","protocol":{"features":["execution_selection"]},"fresh":false,"ready":false,"capabilities":[{"id":"claude","provider":"claude","model":"opus","kind":"claude","available":null,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}]});
    assert!(
        execution_selection::prepare_inventory(&stale, &selection, None)
            .unwrap_err()
            .to_string()
            .contains("offline")
    );
}

#[test]
fn explicit_unknown_auth_is_allowed_but_known_missing_is_rejected() {
    let selection = json!({"selected":{"runtime":"local","capability":"codex"}});
    assert!(execution_selection::prepare_inventory(&inventory(), &selection, None).is_ok());
    let missing = json!({"runtimes":[{"runtime":"controller","local":true,"fresh":true,"ready":true,"capacity":{"available":4,"draining":false},"capabilities":[{"id":"codex","provider":"codex","model":"astra","kind":"codex","available":false,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}]});
    assert!(execution_selection::prepare_inventory(&missing, &selection, None).is_err());
}

#[test]
fn invocation_validates_and_uses_one_local_configuration_snapshot() {
    use horde::config::{Executor, Settings};
    let configured = Settings {
        executors: std::collections::BTreeMap::from([(
            "chosen".into(),
            Executor {
                provider: Some("codex".into()),
                model: Some("astra".into()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let resolved = configured.executor("chosen").unwrap();
    let advertised = json!({"runtimes":[{"runtime":"controller","local":true,"fresh":true,"ready":true,"capacity":{"available":4,"draining":false},"capabilities":[{"id":"chosen","provider":"codex","model":"astra","kind":"codex","available":null,"configuration_hash":horde::capabilities::configuration_hash(&resolved).unwrap()}]}]});
    let policy = execution_selection::prepare_inventory(
        &advertised,
        &json!({"selected":{"runtime":"local","capability":"chosen"}}),
        None,
    )
    .unwrap();
    let inherited = Settings::default();
    let selected = execution_selection::apply_settings(
        &policy,
        "controller",
        &configured,
        &inherited,
        &["thinking".into()],
    )
    .unwrap();
    assert_eq!(
        selected.executor("worker").unwrap().model.as_deref(),
        Some("astra")
    );
    assert_eq!(
        selected.executor("thinking").unwrap().api_key_env,
        resolved.api_key_env
    );
    assert_eq!(
        serde_json::to_value(&configured.executors)
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        1
    );
    assert!(selected.fallbacks.is_empty());
    let changed = Settings {
        executors: std::collections::BTreeMap::from([(
            "chosen".into(),
            Executor {
                model: Some("different".into()),
                ..configured.executors["chosen"].clone()
            },
        )]),
        ..configured.clone()
    };
    assert!(
        execution_selection::apply_settings(&policy, "controller", &changed, &inherited, &[])
            .is_err()
    );
    assert!(
        execution_selection::apply_settings(
            &policy,
            "another-runtime",
            &configured,
            &inherited,
            &[]
        )
        .is_err()
    );
    let changed_endpoint = Settings {
        providers: configured
            .providers
            .iter()
            .map(|(name, provider)| {
                (
                    name.clone(),
                    horde::config::Provider {
                        base_url: "https://changed.example/v1".into(),
                        ..provider.clone()
                    },
                )
            })
            .collect(),
        ..configured.clone()
    };
    assert!(
        execution_selection::apply_settings(
            &policy,
            "controller",
            &changed_endpoint,
            &inherited,
            &[]
        )
        .unwrap_err()
        .to_string()
        .contains("changed")
    );
}

#[test]
fn unpinned_model_defaults_and_legacy_runtime_cannot_accept_scoped_work() {
    for model in [
        json!(null),
        json!("auto"),
        json!("default"),
        json!("AUTO"),
        json!(" auto "),
    ] {
        let unknown = json!({"runtimes":[{"runtime":"controller","local":true,"fresh":true,"ready":true,"capacity":{"available":4,"draining":false},"capabilities":[{"id":"codex","provider":"codex","model":model,"kind":"codex","available":null,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}]});
        assert!(
            execution_selection::prepare_inventory(
                &unknown,
                &json!({"selected":{"runtime":"local","capability":"codex"}}),
                None
            )
            .is_err()
        );
    }
    let legacy = json!({"runtimes":[{"runtime":"worker","fresh":true,"ready":true,"capacity":{"available":4,"draining":false},"capabilities":[{"id":"codex","provider":"codex","model":"astra","kind":"codex","available":null,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}]});
    assert!(
        execution_selection::prepare_inventory(
            &legacy,
            &json!({"selected":{"runtime":"worker","capability":"codex"}}),
            None
        )
        .unwrap_err()
        .to_string()
        .contains("does not advertise")
    );
}

#[test]
fn admission_rejects_full_or_draining_workers_without_falling_back() {
    for capacity in [
        json!({"available":0,"draining":false}),
        json!({"available":4,"draining":true}),
    ] {
        let full = json!({"runtimes":[{"runtime":"controller","local":true,"fresh":true,"ready":true,"capacity":capacity,"capabilities":[{"id":"codex","provider":"codex","model":"astra","kind":"codex","available":null,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}]});
        assert!(
            execution_selection::prepare_inventory(
                &full,
                &json!({"selected":{"runtime":"local","capability":"codex"}}),
                None
            )
            .is_err()
        );
    }
}
