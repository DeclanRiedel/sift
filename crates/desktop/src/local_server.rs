use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sift_client_sdk::Client;

#[derive(Default)]
struct LocalServerState {
    leases: usize,
    child: Option<Child>,
}

impl LocalServerState {
    fn stop_owned_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for LocalServerState {
    fn drop(&mut self) {
        // A configured instance can fail or be cancelled before its first
        // window lease is acquired. Dropping the supervisor still owns cleanup.
        self.stop_owned_child();
    }
}

/// One process-wide supervisor shared by every desktop window. The first
/// lease may start the bundled launcher; the final lease stops only a process
/// that this desktop instance owns.
#[derive(Clone)]
pub struct LocalServerManager {
    state: Arc<Mutex<LocalServerState>>,
    launcher: PathBuf,
    runtime_state_dir: PathBuf,
    base_url: String,
    instance_root: Option<PathBuf>,
    configured_bind: Option<std::net::SocketAddr>,
}

impl LocalServerManager {
    pub fn bundled(runtime_state_dir: PathBuf) -> std::io::Result<Self> {
        let launcher = std::env::current_exe()?
            .parent()
            .ok_or_else(|| std::io::Error::other("desktop executable has no parent directory"))?
            .join(if cfg!(windows) {
                "sift-launcher.exe"
            } else {
                "sift-launcher"
            });
        Ok(Self::new(
            launcher,
            runtime_state_dir,
            "http://127.0.0.1:7474".into(),
        ))
    }

    fn new(launcher: PathBuf, runtime_state_dir: PathBuf, base_url: String) -> Self {
        Self {
            state: Arc::new(Mutex::new(LocalServerState::default())),
            launcher,
            runtime_state_dir,
            base_url,
            instance_root: None,
            configured_bind: None,
        }
    }

    pub fn configured(root: PathBuf) -> Result<Self, String> {
        let server = std::env::current_exe()
            .map_err(|error| format!("resolving desktop executable: {error}"))?
            .parent()
            .ok_or_else(|| "desktop executable has no parent directory".to_string())?
            .join(if cfg!(windows) {
                "sift-server.exe"
            } else {
                "sift-server"
            });
        let instance = sift_server::instance_runtime::InstanceRoot::open(&root)
            .map_err(|error| format!("validating instance root failed: {error:#}"))?;
        let runtime_state_dir = instance.default_state_dir();
        let config = instance
            .runtime_config(&runtime_state_dir)
            .map_err(|error| {
                format!("resolving instance runtime configuration failed: {error:#}")
            })?;
        Ok(Self {
            state: Arc::new(Mutex::new(LocalServerState::default())),
            launcher: server,
            runtime_state_dir,
            base_url: "auto-loopback".into(),
            configured_bind: Some(
                config
                    .bind
                    .parse()
                    .map_err(|_| "invalid configured bind address")?,
            ),
            instance_root: Some(instance.root),
        })
    }

    pub fn acquire(&self) -> LocalServerLease {
        self.state
            .lock()
            .expect("local server lock poisoned")
            .leases += 1;
        LocalServerLease {
            state: self.state.clone(),
        }
    }

    pub fn instance_root(&self) -> Option<&std::path::Path> {
        self.instance_root.as_deref()
    }

    pub async fn ensure_ready(&self) -> Result<Client, String> {
        tokio::time::timeout(Duration::from_secs(10), self.ensure_ready_inner())
            .await
            .map_err(|_| "local Sift server missed the 10-second readiness deadline".to_string())?
    }

    async fn ensure_ready_inner(&self) -> Result<Client, String> {
        if let Some(client) = self.discover_configured_client().await? {
            return Ok(client);
        }
        let local_client = self
            .instance_root
            .is_none()
            .then(|| Client::new(&self.base_url));
        if let Some(client) = &local_client {
            if tokio::time::timeout(Duration::from_secs(1), client.connect())
                .await
                .is_ok_and(|result| result.is_ok())
            {
                return Ok(client.clone());
            }
        }
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "local server lock poisoned")?;
            if state.child.is_none() {
                if !self.launcher.is_file() {
                    return Err(format!(
                        "bundled local server launcher is missing: {}",
                        self.launcher.display()
                    ));
                }
                let mut command = Command::new(&self.launcher);
                if let Some(root) = &self.instance_root {
                    command.arg("--instance-root").arg(root);
                } else {
                    command
                        .args(["--mode", "daemon"])
                        .env("SIFT_RUNTIME__STATE_DIR", &self.runtime_state_dir);
                }
                let child = command
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .map_err(|error| format!("starting local Sift server: {error}"))?;
                state.child = Some(child);
            }
        }

        for _ in 0..100 {
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "local server lock poisoned")?;
                if let Some(child) = state.child.as_mut() {
                    if let Some(status) = child
                        .try_wait()
                        .map_err(|error| format!("checking local Sift server: {error}"))?
                    {
                        state.child = None;
                        return Err(format!("local Sift server exited during startup: {status}"));
                    }
                }
            }
            if let Some(candidate) = self.discover_configured_client().await? {
                return Ok(candidate);
            }
            if let Some(candidate) = &local_client {
                if tokio::time::timeout(Duration::from_secs(1), candidate.connect())
                    .await
                    .is_ok_and(|result| result.is_ok())
                {
                    return Ok(candidate.clone());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("local Sift server missed the 10-second readiness deadline".into())
    }

    async fn discover_configured_client(&self) -> Result<Option<Client>, String> {
        let Some(_) = &self.instance_root else {
            return Ok(None);
        };
        let descriptor = match sift_server::runtime::read_daemon_descriptor(&self.runtime_state_dir)
        {
            Ok(descriptor) => descriptor,
            Err(_) => return Ok(None),
        };
        let endpoint = local_connect_endpoint(
            self.configured_bind.expect("configured manager has a bind"),
            descriptor.endpoint,
        )?;
        let candidate = Client::new(format!("http://{endpoint}"));
        match tokio::time::timeout(Duration::from_secs(1), candidate.connect()).await {
            Ok(Ok(handshake))
                if handshake.instance_id == descriptor.instance_id
                    && handshake.daemon_generation == descriptor.daemon_generation =>
            {
                Ok(Some(candidate))
            }
            _ => Ok(None),
        }
    }

    #[cfg(test)]
    fn lease_count(&self) -> usize {
        self.state.lock().unwrap().leases
    }
}

fn local_connect_endpoint(
    configured: std::net::SocketAddr,
    advertised: std::net::SocketAddr,
) -> Result<std::net::SocketAddr, String> {
    if configured.ip() != advertised.ip()
        || advertised.port() == 0
        || (configured.port() != 0 && configured.port() != advertised.port())
    {
        return Err("local Sift descriptor does not match the configured bind address".into());
    }
    // Network-hosted instances may bind every interface. A local supervisor
    // connects through loopback instead of using a wildcard as a destination.
    let mut endpoint = advertised;
    if endpoint.ip().is_unspecified() {
        endpoint.set_ip(if endpoint.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    Ok(endpoint)
}

pub struct LocalServerLease {
    state: Arc<Mutex<LocalServerState>>,
}

impl Drop for LocalServerLease {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.leases = state.leases.saturating_sub(1);
        if state.leases == 0 {
            state.stop_owned_child();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_demo_resolves_auto_loopback_and_discovers_assigned_port() {
        let directory = tempfile::tempdir().unwrap();
        let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/reproducible-instance");
        for name in ["sift.toml", "sift.lock"] {
            std::fs::copy(demo.join(name), directory.path().join(name)).unwrap();
        }

        let manager = LocalServerManager::configured(directory.path().to_path_buf()).unwrap();
        let bind = manager.configured_bind.unwrap();
        assert_eq!(bind, "127.0.0.1:0".parse().unwrap());
        assert_eq!(
            local_connect_endpoint(bind, "127.0.0.1:7474".parse().unwrap()).unwrap(),
            "127.0.0.1:7474".parse().unwrap()
        );
        assert!(local_connect_endpoint(bind, "192.0.2.1:7474".parse().unwrap()).is_err());
    }

    #[test]
    fn local_network_binds_use_validated_descriptor_endpoints() {
        for (bind, advertised, expected) in [
            ("0.0.0.0:0", "0.0.0.0:7474", "127.0.0.1:7474"),
            ("[::]:7474", "[::]:7474", "[::1]:7474"),
            ("192.0.2.1:7474", "192.0.2.1:7474", "192.0.2.1:7474"),
        ] {
            assert_eq!(
                local_connect_endpoint(bind.parse().unwrap(), advertised.parse().unwrap()).unwrap(),
                expected.parse().unwrap()
            );
        }
        assert!(local_connect_endpoint(
            "127.0.0.1:7474".parse().unwrap(),
            "192.0.2.1:7474".parse().unwrap()
        )
        .is_err());
        assert!(local_connect_endpoint(
            "127.0.0.1:7474".parse().unwrap(),
            "127.0.0.1:1234".parse().unwrap()
        )
        .is_err());
    }

    #[tokio::test]
    async fn a_stalled_http_listener_cannot_block_local_activation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let manager = LocalServerManager::new(
            "missing-launcher".into(),
            "unused-state".into(),
            format!("http://{}", listener.local_addr().unwrap()),
        );
        let stalled = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let result = tokio::time::timeout(Duration::from_secs(3), manager.ensure_ready()).await;
        stalled.abort();
        let error = result
            .expect("probe must have its own response deadline")
            .err()
            .unwrap();
        assert!(error.contains("missing"));
    }

    #[test]
    fn multiple_window_leases_share_one_lifecycle() {
        let manager = LocalServerManager::new(
            "missing-launcher".into(),
            "unused-state".into(),
            "http://127.0.0.1:9".into(),
        );
        let first = manager.acquire();
        let second = manager.acquire();
        assert_eq!(manager.lease_count(), 2);
        drop(first);
        assert_eq!(manager.lease_count(), 1);
        drop(second);
        assert_eq!(manager.lease_count(), 0);
    }
}
