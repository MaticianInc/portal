use serde::{Deserialize, Serialize};

/// Supported JWT HMAC/signing algorithms
#[derive(Debug, Clone, Copy)]
pub enum Algorithm {
    /// HMAC using SHA-256
    HS256,
    /// ECDSA using P-256 and SHA-256
    ES256,
}

/// A basic JWT header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typ: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedClaims {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nbf: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exp: Option<u64>,
}
