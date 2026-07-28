use schemars::schema_for;
use sift_extension_protocol::{ExtensionManifest, Message};

#[test]
fn golden_rpc_frame_round_trips() {
    let fixture = include_str!("fixtures/heartbeat.json").trim();
    let message: Message = serde_json::from_str(fixture).unwrap();
    assert_eq!(serde_json::to_string(&message).unwrap(), fixture);
}

#[test]
fn golden_manifest_parses() {
    let fixture = include_str!("fixtures/manifest.toml");
    let manifest: ExtensionManifest = toml::from_str(fixture).unwrap();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.id.as_str(), "acme/example");
}

#[test]
fn public_contracts_have_json_schemas() {
    let manifest = serde_json::to_value(schema_for!(ExtensionManifest)).unwrap();
    let rpc = serde_json::to_value(schema_for!(Message)).unwrap();
    assert_eq!(manifest["type"], "object");
    assert!(rpc["oneOf"].is_array());
}
