use std::fmt::Display;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A Nexus identifies a single logical connection between host and client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Nexus(u64);

impl Nexus {
    /// Create a `Nexus` from an integer.
    ///
    /// Hosts and clients should not invent new nexus identifier values;
    /// this should only be done when decoding messages from a server.
    pub fn new(raw_id: u64) -> Self {
        Self(raw_id)
    }

    /// Access the `Nexus` value for display purposes.
    ///
    /// This may be needed when building server URLs.
    pub fn raw_id(&self) -> impl Display {
        self.0
    }
}

/// An error parsing a nexus value.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("error parsing nexus id")]
pub struct NexusParseError;

impl FromStr for Nexus {
    type Err = NexusParseError;

    fn from_str(nexus_str: &str) -> Result<Self, Self::Err> {
        let nexus_int: u64 = nexus_str.parse::<u64>().map_err(|_| NexusParseError)?;
        Ok(Nexus(nexus_int))
    }
}
