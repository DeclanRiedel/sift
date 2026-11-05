//! Public workload defaults only; no arbitrary or secret-bearing GUC names.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostgresRoleDefaultScope {
    AllDatabases,
    CurrentDatabase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostgresRoleSetting {
    WorkMem,
    MaintenanceWorkMem,
    StatementTimeout,
    LockTimeout,
    IdleInTransactionSessionTimeout,
    Jit,
    DefaultTransactionReadOnly,
    MaxParallelWorkersPerGather,
}
impl PostgresRoleSetting {
    pub const fn name(self) -> &'static str {
        match self {
            Self::WorkMem => "work_mem",
            Self::MaintenanceWorkMem => "maintenance_work_mem",
            Self::StatementTimeout => "statement_timeout",
            Self::LockTimeout => "lock_timeout",
            Self::IdleInTransactionSessionTimeout => "idle_in_transaction_session_timeout",
            Self::Jit => "jit",
            Self::DefaultTransactionReadOnly => "default_transaction_read_only",
            Self::MaxParallelWorkersPerGather => "max_parallel_workers_per_gather",
        }
    }
    /// Accept fixed public units, without SQL, custom units or session inheritance.
    pub fn native_value(self, value: &str) -> Result<String, &'static str> {
        if matches!(self, Self::Jit | Self::DefaultTransactionReadOnly) {
            return match value {
                "on" | "off" => Ok(value.into()),
                _ => Err("Boolean role defaults require on or off"),
            };
        }
        if value.is_empty() || value.len() > 10 || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Enter an unsigned integer in the displayed native units");
        }
        let number: u32 = value.parse().map_err(|_| "Role default is out of range")?;
        let (minimum, maximum, unit) = match self {
            Self::WorkMem => (64, i32::MAX as u32, "kB"),
            Self::MaintenanceWorkMem => (1024, i32::MAX as u32, "kB"),
            Self::MaxParallelWorkersPerGather => (0, 1024, ""),
            _ => (0, i32::MAX as u32, "ms"),
        };
        if !(minimum..=maximum).contains(&number) {
            return Err("Role default exceeds the setting's native range");
        }
        Ok(format!("{number}{unit}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresRoleDefault {
    pub setting: PostgresRoleSetting,
    pub scope: PostgresRoleDefaultScope,
    pub value: String,
    #[serde(default)]
    pub value_truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_defaults_use_fixed_units_and_refuse_sql_or_secret_names() {
        assert_eq!(
            PostgresRoleSetting::WorkMem.native_value("64").unwrap(),
            "64kB"
        );
        assert_eq!(
            PostgresRoleSetting::StatementTimeout
                .native_value("0")
                .unwrap(),
            "0ms"
        );
        for value in ["", "1MB", "-1", "64'; SELECT 1", "2147483648"] {
            assert!(PostgresRoleSetting::WorkMem.native_value(value).is_err());
        }
        assert!(PostgresRoleSetting::WorkMem.native_value("63").is_err());
        assert!(PostgresRoleSetting::MaintenanceWorkMem
            .native_value("1023")
            .is_err());
        assert!(PostgresRoleSetting::MaxParallelWorkersPerGather
            .native_value("1025")
            .is_err());
        assert!(PostgresRoleSetting::Jit
            .native_value("true; SET role=x")
            .is_err());
        assert_eq!(PostgresRoleSetting::Jit.native_value("off").unwrap(), "off");
        for name in [
            "primary_conninfo",
            "search_path",
            "session_preload_libraries",
            "app.password",
        ] {
            assert!(
                serde_json::from_value::<PostgresRoleSetting>(serde_json::json!(name)).is_err()
            );
        }
    }
}
