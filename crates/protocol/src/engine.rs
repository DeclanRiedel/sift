//! Which database engine a connection / driver / value belongs to.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Postgres,
    SqlServer,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Postgres => "postgres",
            Engine::SqlServer => "sql_server",
        }
    }

    /// Provider id used by the two bundled adapters. `Engine` remains an
    /// internal/native-type discriminator; public capability selection uses
    /// this provider identity.
    pub fn provider_id(self) -> crate::ProviderId {
        crate::ProviderId::new(match self {
            Engine::Postgres => "sift/postgres",
            Engine::SqlServer => "sift/sql-server",
        })
        .expect("bundled provider ids are valid")
    }

    pub fn provider_ref(self, provider_version: impl Into<String>) -> crate::ProviderRef {
        let dialect_id = crate::DialectId::new(match self {
            Engine::Postgres => "sift/postgresql",
            Engine::SqlServer => "sift/tsql",
        })
        .expect("bundled dialect ids are valid");
        crate::ProviderRef {
            provider_id: self.provider_id(),
            dialect_id,
            provider_version: provider_version.into(),
        }
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Engine {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "postgres" | "postgresql" | "pg" => Ok(Engine::Postgres),
            "sql_server" | "sqlserver" | "mssql" => Ok(Engine::SqlServer),
            other => Err(format!("unknown engine: {other}")),
        }
    }
}
