use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde_json::{json, Value};
use sift_client_sdk::Client;
use sift_instance_config::{LockFile, Manifest};
use sift_protocol::{InvokeToolRequest, InvokeToolResponse, ToolContext};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt as _, BufReader};

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_MCP_MESSAGE_BYTES: usize = 1024 * 1024;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("mcp") => {
            let options = McpOptions::parse(&arguments[1..])?;
            let token = read_token(&options.token_file)?;
            let client = Client::new(options.server).with_bearer_token(token);
            serve_mcp(client, options.context).await
        }
        Some("instance") => instance_command(&arguments[1..]).await,
        Some("help" | "--help" | "-h") | None => {
            print_usage();
            Ok(())
        }
        Some(command) => bail!("unknown command `{command}`; run `sift --help`"),
    }
}

const MANIFEST_FILE: &str = "sift.toml";
const LOCK_FILE: &str = "sift.lock";
const MAX_FILE_BYTES: u64 = 1024 * 1024;

async fn instance_command(arguments: &[String]) -> anyhow::Result<()> {
    let Some(command) = arguments.first().map(String::as_str) else {
        print_instance_usage();
        return Ok(());
    };
    match command {
        "new" => instance_new(&arguments[1..]),
        "fmt" => {
            let root = exactly_one_root(&arguments[1..], "fmt")?;
            let manifest = load_manifest(&root)?;
            write_atomic(
                &root.join(MANIFEST_FILE),
                manifest.to_toml_pretty()?.as_bytes(),
            )?;
            println!("formatted {}", root.join(MANIFEST_FILE).display());
            Ok(())
        }
        "validate" => {
            let root = exactly_one_root(&arguments[1..], "validate")?;
            let manifest = load_manifest(&root)?;
            let digest = manifest.configuration_digest()?;
            println!("valid {} ({digest})", root.join(MANIFEST_FILE).display());
            Ok(())
        }
        "lock" => {
            let root = exactly_one_root(&arguments[1..], "lock")?;
            let manifest = load_manifest(&root)?;
            let lock = LockFile::generate(
                &manifest,
                sift_server::VERSION,
                sift_protocol::PROTOCOL_VERSION_NUMBER,
            )?;
            write_generated(&root.join(LOCK_FILE), lock.to_toml_pretty()?.as_bytes())?;
            println!("locked {}", root.join(LOCK_FILE).display());
            Ok(())
        }
        "inspect" => {
            let root = exactly_one_root(&arguments[1..], "inspect")?;
            let (manifest, lock) = load_pair(&root)?;
            let plan = manifest.static_plan(&lock)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
            Ok(())
        }
        "plan" => {
            let root = exactly_one_root(&arguments[1..], "plan")?;
            let (manifest, lock) = load_pair(&root)?;
            let plan = manifest.static_plan(&lock)?;
            let output = json!({
                "scope": "static",
                "note": "destination-aware create/update/delete actions require the apply engine",
                "plan": plan,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(())
        }
        "apply" => {
            let options = InstanceDestinationOptions::parse(&arguments[1..], true)?;
            let instance = sift_server::instance_runtime::InstanceRoot::open(&options.root)?;
            let state_dir = options
                .state_dir
                .unwrap_or_else(|| instance.default_state_dir());
            let report = instance.apply(&state_dir, options.allow_destroy).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "generations" => {
            let options = InstanceDestinationOptions::parse(&arguments[1..], false)?;
            let instance = sift_server::instance_runtime::InstanceRoot::open(&options.root)?;
            let state_dir = options
                .state_dir
                .unwrap_or_else(|| instance.default_state_dir());
            println!(
                "{}",
                serde_json::to_string_pretty(&instance.generations(&state_dir)?)?
            );
            Ok(())
        }
        "status" => {
            let options = InstanceDestinationOptions::parse(&arguments[1..], false)?;
            let applied = sift_server::instance_runtime::load_applied_instance(
                &options.root,
                options.state_dir.as_deref(),
            )?;
            let descriptor_path = applied.config.runtime_state_dir().join("daemon.json");
            let descriptor = match std::fs::symlink_metadata(&descriptor_path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        bail!("daemon descriptor must be a regular non-symlink file");
                    }
                    Some(sift_server::runtime::read_daemon_descriptor(
                        &applied.config.runtime_state_dir(),
                    )?)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error).context("inspecting daemon descriptor"),
            };
            let running = if let Some(descriptor) = &descriptor {
                let endpoint = format!("http://{}", descriptor.endpoint);
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    Client::new(endpoint).connect(),
                )
                .await
                .is_ok_and(|result| {
                    result.is_ok_and(|handshake| {
                        handshake.instance_id == descriptor.instance_id
                            && handshake.daemon_generation == descriptor.daemon_generation
                    })
                })
            } else {
                false
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "running": running,
                    "applied_generation": applied.generation.generation,
                    "daemon": descriptor,
                }))?
            );
            Ok(())
        }
        "credentials" => credential_command(&arguments[1..]).await,
        "help" | "--help" | "-h" => {
            print_instance_usage();
            Ok(())
        }
        other => bail!("unknown instance command `{other}`; run `sift instance --help`"),
    }
}

async fn credential_command(arguments: &[String]) -> anyhow::Result<()> {
    let action = arguments
        .first()
        .context("credentials requires `status` or `import`")?;
    match action.as_str() {
        "status" => {
            let options = InstanceDestinationOptions::parse(&arguments[1..], false)?;
            let applied = sift_server::instance_runtime::load_applied_instance(
                &options.root,
                options.state_dir.as_deref(),
            )?;
            sift_server::instance_runtime::ensure_file_secret_key(&applied.config)?;
            let store = sift_server::metadata_runtime::build_metadata_store(&applied.config)?
                .context("instance configuration unexpectedly disabled metadata")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&store.verified_instance_credential_status().await?)?
            );
            Ok(())
        }
        "import" => {
            let (options, slot, source) = CredentialImportOptions::parse(&arguments[1..])?;
            let applied = sift_server::instance_runtime::load_applied_instance(
                &options.root,
                options.state_dir.as_deref(),
            )?;
            sift_server::instance_runtime::ensure_file_secret_key(&applied.config)?;
            let _maintenance = sift_server::runtime::acquire_maintenance_exclusive(&applied.config)
                .context("stop the instance before importing credentials")?;
            let store = sift_server::metadata_runtime::build_metadata_store(&applied.config)?
                .context("instance configuration unexpectedly disabled metadata")?;
            let input = read_credential_input(source)?;
            let value: Value = serde_json::from_slice(&input)
                .context("credential input must be a typed JSON object")?;
            store.import_instance_credential(&slot, &value).await?;
            println!("imported credential slot {slot}");
            Ok(())
        }
        other => bail!("unknown credentials command `{other}`; expected `status` or `import`"),
    }
}

#[derive(Debug)]
struct InstanceDestinationOptions {
    root: PathBuf,
    state_dir: Option<PathBuf>,
    allow_destroy: bool,
}

impl InstanceDestinationOptions {
    fn parse(arguments: &[String], accept_destroy: bool) -> anyhow::Result<Self> {
        let mut root = None;
        let mut state_dir = None;
        let mut allow_destroy = false;
        let mut index = 0;
        while index < arguments.len() {
            match arguments[index].as_str() {
                "--state-dir" => {
                    index += 1;
                    let value = arguments
                        .get(index)
                        .context("--state-dir requires a path")?;
                    if state_dir.replace(PathBuf::from(value)).is_some() {
                        bail!("--state-dir may be specified only once");
                    }
                }
                "--allow-destroy" if accept_destroy && !allow_destroy => allow_destroy = true,
                value if value.starts_with('-') => bail!("unknown instance option `{value}`"),
                value => {
                    if root.replace(PathBuf::from(value)).is_some() {
                        bail!("command accepts exactly one server root");
                    }
                }
            }
            index += 1;
        }
        Ok(Self {
            root: root.context("command requires a server root")?,
            state_dir,
            allow_destroy,
        })
    }
}

enum CredentialInput {
    Stdin,
    File(PathBuf),
}

struct CredentialImportOptions;

impl CredentialImportOptions {
    fn parse(
        arguments: &[String],
    ) -> anyhow::Result<(InstanceDestinationOptions, String, CredentialInput)> {
        let mut destination_arguments = Vec::new();
        let mut slot = None;
        let mut source = None;
        let mut index = 0;
        while index < arguments.len() {
            match arguments[index].as_str() {
                "--slot" => {
                    index += 1;
                    let value = arguments.get(index).context("--slot requires a slot id")?;
                    if slot.replace(value.clone()).is_some() {
                        bail!("--slot may be specified only once");
                    }
                }
                "--stdin" => {
                    if source.replace(CredentialInput::Stdin).is_some() {
                        bail!("choose exactly one credential input");
                    }
                }
                "--file" => {
                    index += 1;
                    let value = arguments.get(index).context("--file requires a path")?;
                    if source
                        .replace(CredentialInput::File(PathBuf::from(value)))
                        .is_some()
                    {
                        bail!("choose exactly one credential input");
                    }
                }
                value => {
                    destination_arguments.push(value.to_owned());
                    if value == "--state-dir" {
                        index += 1;
                        destination_arguments.push(
                            arguments
                                .get(index)
                                .context("--state-dir requires a path")?
                                .clone(),
                        );
                    }
                }
            }
            index += 1;
        }
        Ok((
            InstanceDestinationOptions::parse(&destination_arguments, false)?,
            slot.context("credentials import requires --slot")?,
            source.context("credentials import requires --stdin or --file")?,
        ))
    }
}

fn read_credential_input(source: CredentialInput) -> anyhow::Result<Vec<u8>> {
    const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    match source {
        CredentialInput::Stdin => {
            std::io::stdin()
                .take(MAX_CREDENTIAL_BYTES + 1)
                .read_to_end(&mut bytes)
                .context("reading credential JSON from stdin")?;
        }
        CredentialInput::File(path) => {
            let metadata = std::fs::symlink_metadata(&path)
                .with_context(|| format!("reading metadata for {}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("credential input must be a regular non-symlink file");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o077 != 0 {
                    bail!("credential input file must not be accessible by group or other users");
                }
            }
            std::fs::File::open(&path)
                .with_context(|| format!("opening {}", path.display()))?
                .take(MAX_CREDENTIAL_BYTES + 1)
                .read_to_end(&mut bytes)
                .with_context(|| format!("reading {}", path.display()))?;
        }
    }
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        bail!("credential input exceeds the {MAX_CREDENTIAL_BYTES}-byte limit");
    }
    Ok(bytes)
}

fn instance_new(arguments: &[String]) -> anyhow::Result<()> {
    let mut root = None;
    let mut name = "my-sift".to_owned();
    let mut github_subject = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--name" => {
                index += 1;
                name = arguments
                    .get(index)
                    .context("--name requires a value")?
                    .clone();
            }
            "--github-subject" => {
                index += 1;
                github_subject = Some(
                    arguments
                        .get(index)
                        .context("--github-subject requires a value")?
                        .clone(),
                );
            }
            value if value.starts_with('-') => bail!("unknown instance new option `{value}`"),
            value => {
                if root.replace(PathBuf::from(value)).is_some() {
                    bail!("instance new accepts exactly one server root");
                }
            }
        }
        index += 1;
    }
    let root = root.context("instance new requires a server root")?;
    let github_subject = github_subject.context("instance new requires --github-subject")?;
    if github_subject.is_empty()
        || github_subject.starts_with('0')
        || !github_subject.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("--github-subject must be a positive numeric GitHub user id");
    }
    std::fs::create_dir_all(&root)
        .with_context(|| format!("creating server root {}", root.display()))?;
    reject_if_exists(&root.join(MANIFEST_FILE))?;
    reject_if_exists(&root.join(LOCK_FILE))?;
    let manifest_id = uuid::Uuid::new_v4();
    let source = new_manifest_source(manifest_id, &name, &github_subject);
    let manifest = Manifest::parse(&source)?;
    let lock = LockFile::generate(
        &manifest,
        sift_server::VERSION,
        sift_protocol::PROTOCOL_VERSION_NUMBER,
    )?;
    write_new(
        &root.join(MANIFEST_FILE),
        manifest.to_toml_pretty()?.as_bytes(),
    )?;
    if let Err(error) = write_new(&root.join(LOCK_FILE), lock.to_toml_pretty()?.as_bytes()) {
        let _ = std::fs::remove_file(root.join(MANIFEST_FILE));
        return Err(error);
    }
    println!("created reproducible Sift instance at {}", root.display());
    println!(
        "next: review sift.toml, then run `sift instance plan {}`",
        root.display()
    );
    Ok(())
}

fn new_manifest_source(manifest_id: uuid::Uuid, name: &str, github_subject: &str) -> String {
    format!(
        r#"kind = "sift-instance"
format_version = 1
manifest_id = "{manifest_id}"
name = "{name}"

[compatibility]
sift = ">=0.1,<0.2"

[server]
deployment = "personal"
transport = "loopback"
mode = "daemon"
bind = "auto-loopback"

[server.metadata]
secret_backend = "file"
store_sql = false

[automation]
unattended_apply = "disabled"

[auth.github]
flow = "local-device"

[auth.admission]
mode = "allowlist"

[[identity.github_principals]]
name = "operator"
subject = "{github_subject}"
instance_admin = true
bootstrap = true

[[tenants]]
name = "default"

[[tenants.memberships]]
principal = "operator"
role = "owner"

[[connections]]
name = "default/postgres"
tenant = "default"
provider = "postgres"
connection_string = "postgresql://sift@127.0.0.1:5432/postgres?sslmode=prefer"
credential_mode = "shared"
credential = "credential:default/postgres/shared"
enabled = true

[connections.policy]
allow_sql = true
allow_schema_read = true
allow_export = false

[connections.lifecycle]
prevent_destroy = true
"#
    )
}

fn exactly_one_root(arguments: &[String], command: &str) -> anyhow::Result<PathBuf> {
    if arguments.len() != 1 {
        bail!("instance {command} requires exactly one server root");
    }
    Ok(PathBuf::from(&arguments[0]))
}

fn load_manifest(root: &Path) -> anyhow::Result<Manifest> {
    let source = read_bounded_regular_file(&root.join(MANIFEST_FILE))?;
    Manifest::parse(&source).context("validating sift.toml")
}

fn load_pair(root: &Path) -> anyhow::Result<(Manifest, LockFile)> {
    let manifest = load_manifest(root)?;
    let source = read_bounded_regular_file(&root.join(LOCK_FILE))?;
    let lock = LockFile::parse(&source).context("parsing sift.lock")?;
    lock.verify(&manifest).context("verifying sift.lock")?;
    Ok((manifest, lock))
}

fn read_bounded_regular_file(path: &Path) -> anyhow::Result<String> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading metadata for {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a regular non-symlink file", path.display());
    }
    if metadata.len() > MAX_FILE_BYTES {
        bail!(
            "{} exceeds the {}-byte limit",
            path.display(),
            MAX_FILE_BYTES
        );
    }
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        bail!(
            "{} changed while reading or exceeds the byte limit",
            path.display()
        );
    }
    String::from_utf8(bytes).with_context(|| format!("{} must be UTF-8", path.display()))
}

fn reject_if_exists(path: &Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => bail!("refusing to overwrite {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("checking {}", path.display())),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", path.display()))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .context("configuration path has no parent directory")?;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading metadata for {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a regular non-symlink file", path.display());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temporary file in {}", parent.display()))?;
    temporary
        .as_file_mut()
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .with_context(|| format!("writing replacement for {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replacing {}", path.display()))?;
    sync_parent(parent)
}

fn write_generated(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => write_atomic(path, bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => write_new(path, bytes),
        Err(error) => Err(error).with_context(|| format!("checking {}", path.display())),
    }
}

fn sync_parent(parent: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("syncing directory {}", parent.display()))?;
    }
    Ok(())
}

fn print_usage() {
    println!(
        "sift instance <command>\nsift mcp --server <url> --token-file <path> [context options]"
    );
    print_instance_usage();
}

fn print_instance_usage() {
    println!(
        "\nInstance configuration:\n\
         sift instance new <server-root> --github-subject <numeric-id> [--name <logical-name>]\n\
         sift instance fmt <server-root>\n\
         sift instance validate <server-root>\n\
         sift instance lock <server-root>\n\
         sift instance inspect <server-root>\n\
         sift instance plan <server-root>\n\
         sift instance apply <server-root> [--state-dir <path>] [--allow-destroy]\n\
         sift instance generations <server-root> [--state-dir <path>]\n\
         sift instance status <server-root> [--state-dir <path>]\n\
         sift instance credentials status <server-root> [--state-dir <path>]\n\
         sift instance credentials import <server-root> --slot <id> (--stdin | --file <path>) [--state-dir <path>]"
    );
}

struct McpOptions {
    server: String,
    token_file: PathBuf,
    context: ToolContext,
}

impl McpOptions {
    fn parse(arguments: &[String]) -> anyhow::Result<Self> {
        let mut server = None;
        let mut token_file = None;
        let mut context = ToolContext {
            tenant_id: None,
            room_id: None,
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        let mut index = 0;
        while index < arguments.len() {
            let option = arguments[index].as_str();
            let value = arguments
                .get(index + 1)
                .with_context(|| format!("{option} requires a value"))?;
            match option {
                "--server" => server = Some(value.clone()),
                "--token-file" => token_file = Some(PathBuf::from(value)),
                "--tenant-id" => context.tenant_id = Some(value.parse().context("invalid tenant")?),
                "--room-id" => context.room_id = Some(value.parse().context("invalid room")?),
                "--profile-id" => {
                    context.profile_id = Some(value.parse().context("invalid profile")?)
                }
                "--connection-id" => context.connection_id = Some(value.clone()),
                "--document-id" => context.document_id = Some(value.clone()),
                unknown => bail!("unknown mcp option `{unknown}`"),
            }
            index += 2;
        }
        let server = server.context("--server is required")?;
        if !(server.starts_with("http://") || server.starts_with("https://")) {
            bail!("--server must be an explicit http:// or https:// URL");
        }
        Ok(Self {
            server,
            token_file: token_file.context("--token-file is required")?,
            context,
        })
    }
}

fn read_token(path: &Path) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("reading token-file metadata: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("token file is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("token file must not be accessible by group or other users");
        }
    }
    let token = std::fs::read_to_string(path)
        .with_context(|| format!("reading token file: {}", path.display()))?;
    let token = token.trim().to_owned();
    if token.is_empty() || token.contains(['\r', '\n']) {
        bail!("token file must contain exactly one non-empty token");
    }
    Ok(token)
}

async fn serve_mcp(client: Client, context: ToolContext) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut input = BufReader::new(stdin);
    let mut output = tokio::io::stdout();
    let mut initialized = false;
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let read = input
            .read_until(b'\n', &mut buffer)
            .await
            .context("reading MCP request")?;
        if read == 0 {
            return Ok(());
        }
        if buffer.len() > MAX_MCP_MESSAGE_BYTES {
            write_response(
                &mut output,
                &rpc_error(Value::Null, -32600, "MCP request exceeds the byte limit"),
            )
            .await?;
            continue;
        }
        while matches!(buffer.last(), Some(b'\n' | b'\r')) {
            buffer.pop();
        }
        let request: Value = match serde_json::from_slice(&buffer) {
            Ok(request) => request,
            Err(_) => {
                write_response(&mut output, &rpc_error(Value::Null, -32700, "Parse error")).await?;
                continue;
            }
        };
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            write_response(
                &mut output,
                &rpc_error(request_id(&request), -32600, "Invalid Request"),
            )
            .await?;
            continue;
        };
        let id = request_id(&request);
        if request.get("id").is_none() {
            if method == "notifications/initialized" {
                initialized = true;
            }
            continue;
        }
        let response = match method {
            "initialize" => {
                initialized = true;
                rpc_result(
                    id,
                    json!({
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {
                            "name": "sift",
                            "version": sift_server::VERSION
                        }
                    }),
                )
            }
            "ping" if initialized => rpc_result(id, json!({})),
            "tools/list" if initialized => match client.governed_tools(&context, true).await {
                Ok(tools) => rpc_result(
                    id,
                    json!({
                        "tools": tools.into_iter().map(|tool| json!({
                            "name": tool.id,
                            "title": tool.title,
                            "description": tool.description,
                            "inputSchema": tool.input_schema,
                            "outputSchema": tool.output_schema,
                            "execution": {"taskSupport": "forbidden"}
                        })).collect::<Vec<_>>()
                    }),
                ),
                Err(_) => rpc_error(id, -32603, "Unable to list authorized Sift tools"),
            },
            "tools/call" if initialized => {
                call_tool(&client, &context, id, request.get("params")).await
            }
            _ if !initialized => rpc_error(id, -32002, "Server is not initialized"),
            _ => rpc_error(id, -32601, "Method not found"),
        };
        write_response(&mut output, &response).await?;
    }
}

async fn call_tool(
    client: &Client,
    context: &ToolContext,
    id: Value,
    params: Option<&Value>,
) -> Value {
    let Some(params) = params.and_then(Value::as_object) else {
        return rpc_error(id, -32602, "Invalid tools/call parameters");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "Tool name is required");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return rpc_error(id, -32602, "Tool arguments must be an object");
    }
    let approval_id = params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("sift/approvalId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    match client
        .invoke_tool(&InvokeToolRequest {
            tool_id: name.into(),
            arguments,
            context: context.clone(),
            approval_id,
        })
        .await
    {
        Ok(InvokeToolResponse::Completed { result }) => rpc_result(
            id,
            json!({
                "content": [{"type": "text", "text": result.to_string()}],
                "structuredContent": result,
                "isError": false
            }),
        ),
        Ok(InvokeToolResponse::ApprovalRequired { approval }) => rpc_result(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": "Approval is required through the authenticated Sift client."
                }],
                "structuredContent": {
                    "status": "approval_required",
                    "approval": approval
                },
                "isError": true
            }),
        ),
        Err(_) => rpc_result(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": "Sift rejected or failed the governed tool operation."
                }],
                "isError": true
            }),
        ),
    }
}

fn request_id(request: &Value) -> Value {
    request.get("id").cloned().unwrap_or(Value::Null)
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

async fn write_response(output: &mut tokio::io::Stdout, response: &Value) -> anyhow::Result<()> {
    let mut encoded = serde_json::to_vec(response)?;
    encoded.push(b'\n');
    output.write_all(&encoded).await?;
    output.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_explicit_server_and_token_file() {
        assert!(McpOptions::parse(&[
            "--server".into(),
            "http://127.0.0.1:3000".into(),
            "--token-file".into(),
            "/tmp/token".into(),
        ])
        .is_ok());
        assert!(McpOptions::parse(&["--server".into(), "localhost".into()]).is_err());
    }

    #[test]
    fn instance_new_creates_a_verified_pair_without_secret_values() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("instance");
        instance_new(&[
            root.display().to_string(),
            "--github-subject".into(),
            "12345678".into(),
            "--name".into(),
            "test-sift".into(),
        ])
        .unwrap();
        let (manifest, lock) = load_pair(&root).unwrap();
        lock.verify(&manifest).unwrap();
        let manifest_source = std::fs::read_to_string(root.join(MANIFEST_FILE)).unwrap();
        assert!(!manifest_source.contains("password"));
        assert!(!manifest_source.contains("token"));
    }

    #[cfg(unix)]
    #[test]
    fn instance_reader_rejects_symlinked_config() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("instance");
        std::fs::create_dir(&root).unwrap();
        let outside = directory.path().join("outside.toml");
        std::fs::write(&outside, "not trusted").unwrap();
        symlink(outside, root.join(MANIFEST_FILE)).unwrap();
        assert!(load_manifest(&root)
            .unwrap_err()
            .to_string()
            .contains("non-symlink"));
    }
}
