use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::PortalId;

/// Claims that we use to form a JWT.
///
/// All claims need to be present for a token to be considered valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    /// Subject. Optional in RFC 7519.
    pub sub: String,
    /// Expiration time as UTC timestamp. Required by RFC7519.
    pub exp: usize,
    /// Role: whether the token holder is a tunnel host or client.
    pub role: Role,
    /// The id of the portal that the holder is allowed to access.
    pub portal_id: PortalId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A host offers a tunnel, and waits for a client to connect.
    Host,
    /// A client connects to an existing tunnel.
    Client,
}

impl FromStr for Role {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "host" => Ok(Role::Host),
            "client" => Ok(Role::Client),
            _ => Err("unknown role"),
        }
    }
}
