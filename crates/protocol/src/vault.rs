use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VaultScope {
    Personal,
    Team,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VaultItemKind {
    Connection,
    Login,
    Token,
    SecureNote,
}

impl VaultItemKind {
    pub const fn revealable(self) -> bool {
        !matches!(self, Self::Connection)
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct VaultCapabilities {
    pub inspect: bool,
    pub use_secret: bool,
    pub reveal: bool,
    pub edit: bool,
    pub manage: bool,
}

impl VaultCapabilities {
    pub const OWNER: Self = Self {
        inspect: true,
        use_secret: true,
        reveal: true,
        edit: true,
        manage: true,
    };

    pub fn normalized(mut self) -> Self {
        if self.use_secret || self.reveal || self.edit || self.manage {
            self.inspect = true;
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VaultAction {
    Read,
    Create,
    Update,
    Delete,
    Grant,
    Revoke,
    SetSecret,
    Reveal,
    Restore,
    Test,
    Use,
}
