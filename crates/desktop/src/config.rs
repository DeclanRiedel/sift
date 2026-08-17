use std::path::{Path, PathBuf};

const SERVER_URL_ENV: &str = "SIFT_DESKTOP__SERVER_URL";
const SERVER_NAME_ENV: &str = "SIFT_DESKTOP__SERVER_NAME";
const INSTANCE_ROOT_ENV: &str = "SIFT_DESKTOP__INSTANCE_ROOT";
const BEARER_TOKEN_ENV: &str = "SIFT_DESKTOP__BEARER_TOKEN";
const BEARER_TOKEN_FILE_ENV: &str = "SIFT_DESKTOP__BEARER_TOKEN_FILE";

#[derive(Clone, Default)]
pub struct DesktopConfig {
    pub remote: Option<RemoteServerConfig>,
    pub instance_root: Option<PathBuf>,
}

#[derive(Clone)]
pub struct RemoteServerConfig {
    pub base_url: String,
    pub name: String,
    bearer_token: Option<String>,
}

impl RemoteServerConfig {
    pub fn bearer_token(&self) -> Option<&str> {
        self.bearer_token.as_deref()
    }
}

#[derive(Default)]
struct RawOptions {
    server_url: Option<String>,
    server_name: Option<String>,
    instance_root: Option<PathBuf>,
    bearer_token: Option<String>,
    bearer_token_file: Option<PathBuf>,
}

impl DesktopConfig {
    pub fn load() -> Result<Self, String> {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        let environment = EnvironmentOptions {
            server_url: std::env::var(SERVER_URL_ENV).ok(),
            server_name: std::env::var(SERVER_NAME_ENV).ok(),
            instance_root: std::env::var_os(INSTANCE_ROOT_ENV).map(PathBuf::from),
            bearer_token: std::env::var(BEARER_TOKEN_ENV).ok(),
            bearer_token_file: std::env::var_os(BEARER_TOKEN_FILE_ENV).map(PathBuf::from),
        };
        Self::from_options(&args, environment)
    }

    fn from_options(args: &[String], environment: EnvironmentOptions) -> Result<Self, String> {
        let command_line = parse_args(args)?;
        let command_line_instance = command_line.instance_root.is_some();
        let command_line_remote = command_line.server_url.is_some();
        let raw = RawOptions {
            server_url: command_line.server_url.or_else(|| {
                (!command_line_instance)
                    .then_some(environment.server_url)
                    .flatten()
            }),
            server_name: command_line.server_name.or_else(|| {
                (!command_line_instance)
                    .then_some(environment.server_name)
                    .flatten()
            }),
            instance_root: command_line.instance_root.or_else(|| {
                (!command_line_remote)
                    .then_some(environment.instance_root)
                    .flatten()
            }),
            bearer_token: command_line.bearer_token.or_else(|| {
                (!command_line_instance)
                    .then_some(environment.bearer_token)
                    .flatten()
            }),
            bearer_token_file: command_line.bearer_token_file.or_else(|| {
                (!command_line_instance)
                    .then_some(environment.bearer_token_file)
                    .flatten()
            }),
        };
        build(raw)
    }
}

#[derive(Default)]
struct EnvironmentOptions {
    server_url: Option<String>,
    server_name: Option<String>,
    instance_root: Option<PathBuf>,
    bearer_token: Option<String>,
    bearer_token_file: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<RawOptions, String> {
    let mut options = RawOptions::default();
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        let (name, inline_value) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(name, value)| {
                (name, Some(value))
            });
        let value = match name {
            "--server-url" | "--server-name" | "--instance-root" | "--bearer-token-file" => {
                inline_value
                    .map(str::to_owned)
                    .or_else(|| arguments.next().cloned())
                    .ok_or_else(|| format!("{name} requires a value"))?
            }
            "--help" | "-h" => return Err(usage().into()),
            _ => {
                return Err(format!(
                    "unknown sift-desktop argument `{argument}`\n\n{}",
                    usage()
                ))
            }
        };
        match name {
            "--server-url" => set_once(&mut options.server_url, value, name)?,
            "--server-name" => set_once(&mut options.server_name, value, name)?,
            "--instance-root" => {
                if options
                    .instance_root
                    .replace(PathBuf::from(value))
                    .is_some()
                {
                    return Err(format!("{name} may be specified only once"));
                }
            }
            "--bearer-token-file" => {
                if options
                    .bearer_token_file
                    .replace(PathBuf::from(value))
                    .is_some()
                {
                    return Err(format!("{name} may be specified only once"));
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(options)
}

fn set_once(slot: &mut Option<String>, value: String, name: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{name} may be specified only once"));
    }
    Ok(())
}

fn build(raw: RawOptions) -> Result<DesktopConfig, String> {
    if raw.instance_root.is_some() && raw.server_url.is_some() {
        return Err("set only one of --instance-root and --server-url".into());
    }
    if let Some(root) = raw.instance_root {
        if raw.server_name.is_some()
            || raw.bearer_token.is_some()
            || raw.bearer_token_file.is_some()
        {
            return Err("server name or credentials cannot be used with an instance root".into());
        }
        let instance = sift_server::instance_runtime::InstanceRoot::open(&root)
            .map_err(|error| format!("invalid Sift instance root: {error:#}"))?;
        return Ok(DesktopConfig {
            remote: None,
            instance_root: Some(instance.root),
        });
    }
    let Some(base_url) = raw.server_url else {
        if raw.server_name.is_some()
            || raw.bearer_token.is_some()
            || raw.bearer_token_file.is_some()
        {
            return Err("remote server name or credentials require a server URL".into());
        }
        return Ok(DesktopConfig::default());
    };
    let base_url = validate_base_url(&base_url)?;
    if raw.bearer_token.is_some() && raw.bearer_token_file.is_some() {
        return Err(format!(
            "set only one of {BEARER_TOKEN_ENV} and {BEARER_TOKEN_FILE_ENV}/--bearer-token-file"
        ));
    }
    let bearer_token = match (raw.bearer_token, raw.bearer_token_file) {
        (Some(token), None) => Some(validate_token(token)?),
        (None, Some(path)) => Some(read_token(&path)?),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!(),
    };
    let name = raw
        .server_name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Remote Sift".into());
    Ok(DesktopConfig {
        remote: Some(RemoteServerConfig {
            base_url,
            name,
            bearer_token,
        }),
        instance_root: None,
    })
}

pub(crate) fn validate_base_url(value: &str) -> Result<String, String> {
    let mut url =
        reqwest::Url::parse(value).map_err(|error| format!("invalid server URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("server URL must be an http:// or https:// origin".into());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(
            "server URL must be an origin without credentials, path, query, or fragment".into(),
        );
    }
    url.set_path("");
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn read_token(path: &Path) -> Result<String, String> {
    let token = std::fs::read_to_string(path)
        .map_err(|error| format!("reading bearer token file {}: {error}", path.display()))?;
    validate_token(token.trim_end_matches(['\r', '\n']).to_owned())
}

pub(crate) fn validate_token(token: String) -> Result<String, String> {
    if token.is_empty() {
        return Err("bearer token must not be empty".into());
    }
    if token.contains(['\r', '\n']) {
        return Err("bearer token must be a single line".into());
    }
    if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("bearer token must contain only printable ASCII without spaces".into());
    }
    Ok(token)
}

pub fn usage() -> &'static str {
    "Usage: sift-desktop [--instance-root <folder> | --server-url <http(s)://host:port> [--server-name <name>] [--bearer-token-file <path>]]"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn no_remote_options_preserve_local_default() {
        let config = DesktopConfig::from_options(&[], EnvironmentOptions::default()).unwrap();
        assert!(config.remote.is_none());
        assert!(config.instance_root.is_none());
    }

    #[test]
    fn command_line_server_builds_remote_config() {
        let config = DesktopConfig::from_options(
            &args(&[
                "--server-url=http://192.168.1.10:7474/",
                "--server-name",
                "Lab",
            ]),
            EnvironmentOptions::default(),
        )
        .unwrap();
        let remote = config.remote.unwrap();
        assert_eq!(remote.base_url, "http://192.168.1.10:7474");
        assert_eq!(remote.name, "Lab");
        assert!(remote.bearer_token().is_none());
    }

    #[test]
    fn command_line_overrides_environment_endpoint() {
        let config = DesktopConfig::from_options(
            &args(&["--server-url", "https://sift.lan"]),
            EnvironmentOptions {
                server_url: Some("http://old.lan:7474".into()),
                bearer_token: Some("secret".into()),
                ..EnvironmentOptions::default()
            },
        )
        .unwrap();
        let remote = config.remote.unwrap();
        assert_eq!(remote.base_url, "https://sift.lan");
        assert_eq!(remote.bearer_token(), Some("secret"));
    }

    #[test]
    fn command_line_instance_root_is_validated_and_canonicalized() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reproducible-instance");
        let config = DesktopConfig::from_options(
            &args(&["--instance-root", root.to_str().unwrap()]),
            EnvironmentOptions {
                server_url: Some("https://ignored.example".into()),
                bearer_token: Some("ignored".into()),
                ..EnvironmentOptions::default()
            },
        )
        .unwrap();

        assert!(config.remote.is_none());
        assert_eq!(
            config.instance_root,
            Some(std::fs::canonicalize(root).unwrap())
        );
    }

    #[test]
    fn rejects_unsafe_or_ambiguous_remote_options() {
        for url in [
            "ftp://sift.lan",
            "http://user@sift.lan",
            "http://sift.lan/v1",
            "http://sift.lan?token=secret",
        ] {
            assert!(DesktopConfig::from_options(
                &args(&["--server-url", url]),
                EnvironmentOptions::default()
            )
            .is_err());
        }
        assert!(DesktopConfig::from_options(
            &[],
            EnvironmentOptions {
                bearer_token: Some("secret".into()),
                ..EnvironmentOptions::default()
            }
        )
        .is_err());
        assert!(DesktopConfig::from_options(
            &args(&[
                "--instance-root",
                "/tmp/unused",
                "--server-url",
                "https://sift.lan",
            ]),
            EnvironmentOptions::default(),
        )
        .is_err());
    }
}
