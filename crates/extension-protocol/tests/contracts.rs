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

#[test]
fn estimated_family_since_v3_and_savepoints_remain_inherited() {
    use sift_extension_protocol::*;
    for version in 1..=4 {
        assert_eq!(
            driver_rpc_supports_capability("driver.explain.estimated@1", version),
            version >= 3
        );
        assert_eq!(
            driver_rpc_supports_capability("driver.savepoints@1", version),
            version >= 2
        );
        assert!(!driver_rpc_supports_capability("driver.explain@1", version));
        assert_eq!(
            negotiate_driver_rpc(
                VersionRange {
                    minimum: 1,
                    maximum: 4
                },
                VersionRange {
                    minimum: 1,
                    maximum: version
                }
            ),
            Some(version)
        );
    }
    assert!(negotiate_driver_rpc(
        VersionRange {
            minimum: 3,
            maximum: 3
        },
        VersionRange {
            minimum: 1,
            maximum: 2
        }
    )
    .is_none());
    let schema = serde_json::to_value(schema_for!(DriverEstimatedPlanResponse)).unwrap();
    assert!(schema["properties"]["plan"].is_object());
}

#[test]
fn notifications_require_v4_and_bounded_distinct_utf8_channel_labels() {
    use sift_extension_protocol::*;
    for version in 1..=4 {
        assert_eq!(
            driver_rpc_supports_capability("driver.notifications@1", version),
            version >= 4
        );
    }
    assert!(valid_notification_channels(&[
        "changes".into(),
        "通知".into()
    ]));
    for channels in [
        vec![],
        vec![String::new()],
        vec!["a".into(), "a".into()],
        vec!["a\nb".into()],
        vec!["🦀".repeat(16)],
        vec!["x".into(); 17],
    ] {
        assert!(!valid_notification_channels(&channels));
    }
    let schema = serde_json::to_value(schema_for!(DriverNotificationFrame)).unwrap();
    assert!(schema["oneOf"].is_array());
}
