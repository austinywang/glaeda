use serde_json::Value;

const PREPARED_TEMPLATE: &str = include_str!("../examples/lima/glaeda-prepared-template.yaml");

fn prepared_template() -> Value {
    serde_yaml::from_str(PREPARED_TEMPLATE).expect("prepared Lima template must remain valid YAML")
}

fn empty_sequence(value: &Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
}

#[test]
fn prepared_vz_template_keeps_the_network_selector_observation_baseline() {
    let template = prepared_template();

    assert_eq!(template.get("vmType").and_then(Value::as_str), Some("vz"));
    assert_eq!(
        template.get("arch").and_then(Value::as_str),
        Some("aarch64")
    );
    assert_eq!(template.get("plain").and_then(Value::as_bool), Some(true));

    assert!(empty_sequence(&template, "mounts"));
    assert!(empty_sequence(&template, "networks"));
    assert!(empty_sequence(&template, "portForwards"));

    let ssh = template
        .get("ssh")
        .and_then(Value::as_object)
        .expect("prepared template must define the reviewed SSH boundary");
    assert_eq!(ssh.get("localPort").and_then(Value::as_u64), Some(0));
    assert_eq!(ssh.get("overVsock").and_then(Value::as_bool), Some(true));
    assert_eq!(
        ssh.get("loadDotSSHPubKeys").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        ssh.get("forwardAgent").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(ssh.get("forwardX11").and_then(Value::as_bool), Some(false));
    assert_eq!(
        ssh.get("forwardX11Trusted").and_then(Value::as_bool),
        Some(false)
    );

    let host_resolver = template
        .get("hostResolver")
        .and_then(Value::as_object)
        .expect("prepared template must define host resolver policy");
    assert_eq!(
        host_resolver.get("enabled").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        template.get("propagateProxyEnv").and_then(Value::as_bool),
        Some(false)
    );

    let dns = template
        .get("dns")
        .and_then(Value::as_array)
        .expect("prepared template must pin public DNS resolvers");
    let dns = dns.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    assert_eq!(dns, ["1.1.1.1", "1.0.0.1"]);

    assert!(
        !PREPARED_TEMPLATE.contains("61922"),
        "the retired fixed Lima control-port premise must not return"
    );
}
