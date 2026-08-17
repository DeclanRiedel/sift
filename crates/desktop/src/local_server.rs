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
        Ok(Self {
            state: Arc::new(Mutex::new(LocalServerState::default())),
            launcher: server,
            runtime_state_dir: instance.default_state_dir(),
            base_url: "auto-loopback".into(),
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

    pub async fn ensure_ready(&self) -> Result<Client, String> {
        if let Some(client) = self.discover_configured_client().await? {
            return Ok(client);
        }
        if self.instance_root.is_none() {
            let client = Client::new(&self.base_url);
            if client.connect().await.is_ok() {
                return Ok(client);
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
            if self.instance_root.is_none() {
                let candidate = Client::new(&self.base_url);
                if candidate.connect().await.is_ok() {
                    return Ok(candidate);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("local Sift server missed the 10-second readiness deadline".into())
    }

    async fn discover_configured_client(&self) -> Result<Option<Client>, String> {
        let Some(root) = &self.instance_root else {
            return Ok(None);
        };
        let (_, _, config) = match sift_server::instance_runtime::load_current_config(root, None) {
            Ok(current) => current,
            Err(error) => return Err(format!("loading applied instance failed: {error:#}")),
        };
        let descriptor =
            match sift_server::runtime::read_daemon_descriptor(&config.runtime_state_dir()) {
                Ok(descriptor) => descriptor,
                Err(_) => return Ok(None),
            };
        let candidate = Client::new(format!("http://{}", descriptor.endpoint));
        match candidate.connect().await {
            Ok(handshake)
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
            if let Some(child) = state.child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            state.child = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
