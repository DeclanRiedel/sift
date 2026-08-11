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
        }
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
        let client = Client::new(&self.base_url);
        if client.connect().await.is_ok() {
            return Ok(client);
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
                let child = Command::new(&self.launcher)
                    .args(["--mode", "daemon"])
                    .env("SIFT_RUNTIME__STATE_DIR", &self.runtime_state_dir)
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
            let candidate = Client::new(&self.base_url);
            if candidate.connect().await.is_ok() {
                return Ok(candidate);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("local Sift server missed the 10-second readiness deadline".into())
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
