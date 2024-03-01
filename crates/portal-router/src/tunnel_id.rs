use std::fmt::Display;
use std::str::FromStr;

use serde::Deserialize;

/// An ID used to identify a websocket tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct TunnelId(u64);

impl FromStr for TunnelId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let id: u64 = s.parse().map_err(|_| InvalidId)?;
        Ok(Self(id))
    }
}

// This is used to generate a Durable Object Id.
impl Display for TunnelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use the inner id in decimal format.
        self.0.fmt(f)
    }
}

/// A `TunnelId` failed to parse.
pub struct InvalidId;
