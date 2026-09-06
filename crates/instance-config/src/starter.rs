//! One starter policy for the CLI and desktop; the shipped example supplies defaults.

use crate::{ConfigError, Manifest};
use uuid::Uuid;

/// Create a validated personal instance without interpolating user input into TOML.
/// The caller owns identity generation and must supply an immutable GitHub user ID.
pub fn personal_starter(
    manifest_id: Uuid,
    name: &str,
    github_subject: &str,
) -> Result<Manifest, ConfigError> {
    let mut manifest = Manifest::parse(include_str!(
        "../../../examples/reproducible-instance/sift.toml"
    ))?;
    manifest.manifest_id = manifest_id;
    manifest.name = name.into();
    let operator = &mut manifest.identity.github_principals[0];
    operator.subject = github_subject.into();
    operator.login_hint = None;
    manifest.tenants[0].name = "default".into();
    let connection = &mut manifest.connections[0];
    connection.name = "default/postgres".into();
    connection.tenant = "default".into();
    connection.credential = Some("credential:default/postgres/shared".into());
    connection.tags.clear();
    manifest.normalize();
    manifest.validate()?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_is_round_trippable_and_rejects_injected_identity() {
        let id = Uuid::from_u128(1);
        let manifest = personal_starter(id, "new-sift", "12345678").unwrap();
        assert_eq!(manifest.manifest_id, id);
        assert_eq!(manifest.connections[0].tenant, manifest.tenants[0].name);
        assert_eq!(
            Manifest::parse(&manifest.to_toml_pretty().unwrap()).unwrap(),
            manifest
        );
        assert!(personal_starter(id, "new-sift", "123\"\nbootstrap = true").is_err());
        assert!(personal_starter(id, "new-sift", "github-name").is_err());
    }
}
