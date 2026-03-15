use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::PortalId;

/// The expected issuer value for portal JWTs.
pub const PORTAL_JWT_ISSUER: &str = "matic-portal";
/// The expected audience value for portal JWTs.
pub const PORTAL_JWT_AUDIENCE: &str = "matic-portal-router";

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
    /// Issuer (RFC 7519). Prevents tokens from other services being reused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    /// Audience (RFC 7519). Restricts which services may accept this token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
}

/// A role that defines a token holder as a host or client.
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
