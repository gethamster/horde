use serde_json::{Value, json};

fn inventory() -> Value {
    let runtime = |id: &str, name: &str, local, models: &[&str]| json!({"runtime":id,"name":name,"local":local,"fresh":true,"ready":true,"protocol":{"features":["execution_selection"]},"capacity":{"available":4,"draining":false},"capabilities":models.iter().map(|m|json!({"id":m,"model":m,"provider":m,"kind":"native","configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","available":null,"availability":{"verified":false}})).collect::<Vec<_>>()});
    json!({"runtimes":[runtime("bigmac-id","bigmac",true,&["codex","astra"]),runtime("apollo-id","apollo",false,&["claude","codex","glm-5.3"])]})
}

fn request() -> Value {
    json!({"roles":{"thinking":{"runtime":"local","models":["codex","astra"]},"delivery":{"runtime":"apollo","models":["claude","codex","glm 5.3"]}}})
}

#[test]
fn resolves_the_users_example_as_allowed_pools_without_dispatching_or_guessing_auth() {
    let result = horde::orchestration::resolve(&inventory(), &request()).unwrap();
    assert_eq!(result["ready"], true);
    assert_eq!(result["roles"]["thinking"]["runtime"], "bigmac-id");
    assert_eq!(
        result["roles"]["delivery"]["allowed"][0]["capabilities"],
        json!(["claude", "codex", "glm-5.3"])
    );
    assert_eq!(result["dispatch_started"], false);
    assert!(result["roles"]["delivery"]["choices"][0]["available"].is_null());
}

#[test]
fn blockers_preserve_valid_roles_and_never_substitute() {
    for (field, value, expected) in [
        ("fresh", json!(false), "stale"),
        ("ready", json!(false), "offline"),
    ] {
        let mut inv = inventory();
        inv["runtimes"][1][field] = value;
        let result = horde::orchestration::resolve(&inv, &request()).unwrap();
        assert_eq!(result["ready"], false);
        assert!(result["roles"]["thinking"].is_object());
        assert!(result["roles"]["delivery"].is_null());
        assert!(
            result["blockers"][0]["reason"]
                .as_str()
                .unwrap()
                .contains(expected)
        );
    }
    let mut inv = inventory();
    inv["runtimes"][1]["capacity"]["available"] = json!(0);
    assert_eq!(
        horde::orchestration::resolve(&inv, &request()).unwrap()["ready"],
        false
    );
    let mut inv = inventory();
    inv["runtimes"][1]["capabilities"][2]["available"] = json!(false);
    assert!(
        horde::orchestration::resolve(&inv, &request()).unwrap()["blockers"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("credentials")
    );
}

#[test]
fn unknown_and_ambiguous_names_require_resolution() {
    let mut req = request();
    req["roles"]["delivery"]["models"] = json!(["glm 6"]);
    assert!(
        horde::orchestration::resolve(&inventory(), &req).unwrap()["blockers"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("not advertised")
    );
    let mut inv = inventory();
    let mut duplicate = inv["runtimes"][1].clone();
    duplicate["runtime"] = json!("other");
    inv["runtimes"].as_array_mut().unwrap().push(duplicate);
    assert!(
        horde::orchestration::resolve(&inv, &request()).unwrap()["blockers"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("ambiguous")
    );
    assert!(horde::orchestration::resolve(&inventory(), &json!({"roles":{}})).is_err());
}

#[test]
fn first_use_planning_rejects_unpinned_models_and_old_worker_protocols() {
    let mut inv = inventory();
    inv["runtimes"][1]["capabilities"][0]["model"] = json!(null);
    assert_eq!(
        horde::orchestration::resolve(&inv, &request()).unwrap()["ready"],
        false
    );
    let mut inv = inventory();
    inv["runtimes"][1]["protocol"] = json!({"version":1});
    assert_eq!(
        horde::orchestration::resolve(&inv, &request()).unwrap()["ready"],
        false
    );
}

#[test]
fn model_alias_cannot_hide_different_executor_settings() {
    let mut inv = inventory();
    let mut first = inv["runtimes"][0]["capabilities"][0].clone();
    first["model"] = json!("gpt-test");
    let mut second = first.clone();
    second["id"] = json!("reviewer");
    second["configuration_hash"] =
        json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    inv["runtimes"][0]["capabilities"] = json!([first, second]);
    let req = json!({"roles":{"thinking":{"runtime":"local","models":["gpt-test"]}}});
    assert_eq!(
        horde::orchestration::resolve(&inv, &req).unwrap()["ready"],
        false
    );
    inv["runtimes"][0]["capabilities"][1]["configuration_hash"] =
        json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(
        horde::orchestration::resolve(&inv, &req).unwrap()["ready"],
        true
    );
}
