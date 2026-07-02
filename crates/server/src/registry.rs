//! Driver registry: `Engine` → `Arc<dyn Driver>`. Populated at startup;
//! immutable thereafter (read-only access from session/connection handlers).

use std::collections::HashMap;
use std::sync::Arc;

use sift_driver_api::Driver;
use sift_protocol::{Code, DriverError, Engine};

/// Map of registered drivers, keyed by engine. Cheap to clone (`Arc` inside).
/// Built once at startup; the HTTP layer reads from it via shared reference.
#[derive(Clone, Default)]
pub struct DriverRegistry {
    inner: Arc<HashMap<Engine, Arc<dyn Driver>>>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HashMap::new()),
        }
    }

    pub fn builder() -> DriverRegistryBuilder {
        DriverRegistryBuilder::default()
    }

    pub fn get(&self, engine: Engine) -> Result<Arc<dyn Driver>, DriverError> {
        self.inner.get(&engine).cloned().ok_or_else(|| {
            DriverError::new(
                Code::UnsupportedForEngine,
                format!("no driver registered for engine `{engine}`"),
            )
        })
    }

    pub fn engines(&self) -> Vec<Engine> {
        self.inner.keys().copied().collect()
    }
}

#[derive(Default)]
pub struct DriverRegistryBuilder {
    drivers: Vec<Arc<dyn Driver>>,
}

impl DriverRegistryBuilder {
    pub fn register<D>(mut self, driver: D) -> Self
    where
        D: Driver + 'static,
    {
        let engine = driver.engine();
        let arc: Arc<dyn Driver> = Arc::new(driver);
        tracing::info!(%engine, "registered driver");
        self.drivers.push(arc);
        self
    }

    pub fn build(self) -> DriverRegistry {
        let mut map: HashMap<Engine, Arc<dyn Driver>> = HashMap::new();
        for d in self.drivers {
            map.insert(d.engine(), d);
        }
        DriverRegistry {
            inner: Arc::new(map),
        }
    }
}
