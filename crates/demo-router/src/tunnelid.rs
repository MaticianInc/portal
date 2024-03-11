use std::fmt::Display;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PortalId(u64);

impl From<u64> for PortalId {
    fn from(id: u64) -> Self {
        PortalId(id)
    }
}

impl FromStr for PortalId {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(PortalId(s.parse()?))
    }
}

// The display impl just prints the inner integer.
impl Display for PortalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
