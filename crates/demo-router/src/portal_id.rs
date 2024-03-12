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

/// A service name must be non-empty and may only contain the characters `A-Za-z0-9-_`.
#[derive(Debug, thiserror::Error)]
#[error("improper service name")]
pub struct BadServiceName;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct ServiceName(String);

impl TryFrom<String> for ServiceName {
    type Error = BadServiceName;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        if name.is_empty() {
            return Err(BadServiceName);
        }
        for c in name.chars() {
            match c {
                '0'..='9' | 'A'..='Z' | 'a'..='z' | '-' | '_' => (),
                _ => return Err(BadServiceName),
            }
        }
        Ok(ServiceName(name))
    }
}

// The display impl just prints the inner integer.
impl Display for ServiceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
