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
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => bail!("unknown command `{other}`; run `sift-admin --help`"),
    }
}

async fn bootstrap_admin(args: &[String]) -> anyhow::Result<()> {
    let parsed = BootstrapArgs::parse(args)?;
    let username = normalize_username(&parsed.username)?;
    let password = read_password(parsed.password_stdin)?;
    let verifier = hash_password(password).await?;

    let cfg = load_config().context("loading config")?;
    if !cfg.metadata.enabled {
        bail!("offline administration requires metadata.enabled=true");
    }
    if cfg.metadata.secret_backend == "memory" {
        bail!("offline administration requires a durable secret backend");
    }
    let store = build_metadata_store(&cfg)?
        .context("offline administration requires metadata.enabled=true")?;
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
                unknown => bail!("unknown bootstrap-admin option `{unknown}`"),
            }
            index += 1;
        }
        Ok(parsed)
    }
}

fn print_usage() {
    println!(
        "sift-admin bootstrap-admin <username> [--display-name <name>] [--email <email>] [--password-stdin]\n\
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
