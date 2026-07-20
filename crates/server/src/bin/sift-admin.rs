//! Offline, instance-local administration. Password bytes are accepted only
//! through a hidden TTY prompt or stdin, never command arguments or env vars.

use std::io::{self, IsTerminal, Read};

use anyhow::{bail, Context};
use sift_metadata::{NewOperationAudit, NewPasswordPrincipal};
use sift_server::config::load as load_config;
use sift_server::identity::{hash_password, normalize_username};
use sift_server::metadata_runtime::build_metadata_store;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    match command {
        "bootstrap-admin" => bootstrap_admin(&args[1..]).await,
        "create-user" => create_user(&args[1..]).await,
        "disable-user" => set_user_disabled(&args[1..], true),
        "enable-user" => set_user_disabled(&args[1..], false),
        "reset-password" => reset_password(&args[1..]).await,
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => bail!("unknown command `{other}`; run `sift-admin --help`"),
    }
}

fn durable_store() -> anyhow::Result<sift_metadata::MetadataStore> {
    let cfg = load_config().context("loading config")?;
    if !cfg.metadata.enabled {
        bail!("offline administration requires metadata.enabled=true");
    }
    if cfg.metadata.secret_backend == "memory" {
        bail!("offline administration requires a durable secret backend");
    }
    build_metadata_store(&cfg)?.context("offline administration requires metadata.enabled=true")
}

async fn bootstrap_admin(args: &[String]) -> anyhow::Result<()> {
    let parsed = BootstrapArgs::parse(args)?;
    let username = normalize_username(&parsed.username)?;
    let password = read_password(parsed.password_stdin)?;
    let verifier = hash_password(password).await?;

    let store = durable_store()?;
    if store.has_active_instance_admin()? {
        bail!("an active instance administrator already exists");
    }

    let display_name = parsed.display_name.as_deref().unwrap_or(&username);
    let principal = store
        .create_password_principal(
            NewPasswordPrincipal {
                username: &username,
                display_name,
                email: parsed.email.as_deref(),
                is_instance_admin: true,
            },
            verifier.as_bytes(),
            NewOperationAudit {
                actor_principal_id: None,
                action: "manage_principal.bootstrap_admin".into(),
                target: "principal".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: Some("offline-admin".into()),
            },
        )
        .await
        .context("creating instance administrator")?;

    println!(
        "created instance administrator `{username}` (principal {})",
        principal.id.0
    );
    Ok(())
}

async fn create_user(args: &[String]) -> anyhow::Result<()> {
    let parsed = BootstrapArgs::parse(args)?;
    let username = normalize_username(&parsed.username)?;
    let password = read_password(parsed.password_stdin)?;
    let verifier = hash_password(password).await?;
    let store = durable_store()?;
    if !store.has_active_instance_admin()? {
        bail!("no active instance administrator exists; use bootstrap-admin first");
    }
    let display_name = parsed.display_name.as_deref().unwrap_or(&username);
    let principal = store
        .create_password_principal(
            NewPasswordPrincipal {
                username: &username,
                display_name,
                email: parsed.email.as_deref(),
                is_instance_admin: parsed.admin,
            },
            verifier.as_bytes(),
            offline_audit("manage_principal.create", "principal", None),
        )
        .await
        .context("creating user")?;
    println!("created user `{username}` (principal {})", principal.id.0);
    Ok(())
}

fn set_user_disabled(args: &[String], disabled: bool) -> anyhow::Result<()> {
    let username = single_username_arg(args)?;
    let store = durable_store()?;
    let password = store
        .resolve_password_identity(&username)?
        .with_context(|| format!("password user `{username}` not found"))?;
    let action = if disabled {
        "manage_principal.disable"
    } else {
        "manage_principal.enable"
    };
    store.set_principal_disabled(
        password.principal.id,
        disabled,
        offline_audit(action, "principal", Some(password.principal.id.0)),
    )?;
    println!(
        "{} user `{username}` (principal {})",
        if disabled { "disabled" } else { "enabled" },
        password.principal.id.0
    );
    Ok(())
}

async fn reset_password(args: &[String]) -> anyhow::Result<()> {
    let parsed = PasswordTargetArgs::parse(args)?;
    let username = normalize_username(&parsed.username)?;
    let password = read_password(parsed.password_stdin)?;
    let verifier = hash_password(password).await?;
    let store = durable_store()?;
    let existing = store
        .resolve_password_identity(&username)?
        .with_context(|| format!("password user `{username}` not found"))?;
    store
        .replace_password_verifier(
            existing.identity.id,
            verifier.as_bytes(),
            offline_audit(
                "manage_principal.reset_password",
                "auth_identity",
                Some(existing.identity.id.0),
            ),
        )
        .await?;
    println!("reset password for `{username}`");
    Ok(())
}

fn offline_audit(action: &str, target: &str, target_id: Option<i64>) -> NewOperationAudit {
    NewOperationAudit {
        actor_principal_id: None,
        action: action.into(),
        target: target.into(),
        target_id,
        status: "succeeded".into(),
        result_code: None,
        row_count: None,
        error_message: None,
        correlation_id: Some("offline-admin".into()),
    }
}

fn single_username_arg(args: &[String]) -> anyhow::Result<String> {
    if args.len() != 1 {
        bail!("command requires exactly one username");
    }
    normalize_username(&args[0])
}

fn read_password(force_stdin: bool) -> anyhow::Result<Vec<u8>> {
    if !force_stdin && io::stdin().is_terminal() {
        let first = rpassword::prompt_password("Password: ").context("reading password")?;
        let second = rpassword::prompt_password("Confirm password: ")
            .context("reading password confirmation")?;
        if first != second {
            bail!("password confirmation does not match");
        }
        return Ok(first.into_bytes());
    }

    let mut bytes = Vec::new();
    io::stdin()
        .take(1026)
        .read_to_end(&mut bytes)
        .context("reading password from stdin")?;
    if bytes.ends_with(b"\n") {
        bytes.pop();
        if bytes.ends_with(b"\r") {
            bytes.pop();
        }
    }
    Ok(bytes)
}

struct BootstrapArgs {
    username: String,
    display_name: Option<String>,
    email: Option<String>,
    password_stdin: bool,
    admin: bool,
}

impl BootstrapArgs {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let Some(username) = args.first() else {
            bail!("bootstrap-admin requires a username");
        };
        if username.starts_with('-') {
            bail!("bootstrap-admin requires the username as its first argument");
        }
        let mut parsed = Self {
            username: username.clone(),
            display_name: None,
            email: None,
            password_stdin: false,
            admin: false,
        };
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--display-name" => {
                    index += 1;
                    parsed.display_name = Some(
                        args.get(index)
                            .context("--display-name requires a value")?
                            .clone(),
                    );
                }
                "--email" => {
                    index += 1;
                    parsed.email =
                        Some(args.get(index).context("--email requires a value")?.clone());
                }
                "--password-stdin" => parsed.password_stdin = true,
                "--admin" => parsed.admin = true,
                unknown => bail!("unknown bootstrap-admin option `{unknown}`"),
            }
            index += 1;
        }
        Ok(parsed)
    }
}

struct PasswordTargetArgs {
    username: String,
    password_stdin: bool,
}

impl PasswordTargetArgs {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        if args.is_empty() || args.len() > 2 {
            bail!("reset-password requires a username and optional --password-stdin");
        }
        if args.len() == 2 && args[1] != "--password-stdin" {
            bail!("unknown reset-password option `{}`", args[1]);
        }
        Ok(Self {
            username: args[0].clone(),
            password_stdin: args.len() == 2,
        })
    }
}

fn print_usage() {
    println!(
        "sift-admin bootstrap-admin <username> [--display-name <name>] [--email <email>] [--password-stdin]\n\
         sift-admin create-user <username> [--display-name <name>] [--email <email>] [--admin] [--password-stdin]\n\
         sift-admin disable-user <username>\n\
         sift-admin enable-user <username>\n\
         sift-admin reset-password <username> [--password-stdin]\n\
         \nPassword input defaults to a hidden TTY prompt. With --password-stdin (or redirected stdin),\n\
         one newline-terminated password is read from stdin. Password arguments and environment\n\
         variables are intentionally unsupported."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_has_no_password_value_option() {
        let args = vec!["alice".into(), "--password".into(), "secret".into()];
        assert!(BootstrapArgs::parse(&args).is_err());
    }
}
