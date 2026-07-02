//! Which database engine a connection / driver / value belongs to.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
