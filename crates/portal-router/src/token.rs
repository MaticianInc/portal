use jwt_webcrypto::{Algorithm, Validator};
use serde::Deserialize;

use crate::tunnel_id::TunnelId;

#[derive(Debug, Clone, Copy)]
pub struct ValidationError;

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The token holder will act as a tunnel host.
    Host,
    /// The token holder will act as a tunnel client.
    Client,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    /// Subject: who the token was issued to.
    pub sub: String,
    /// Role: whether the token holder is a tunnel host or client.
    pub role: Role,
    /// The id of the tunnel that the holder is allowed to access.
    pub tunnel_id: TunnelId,
}

pub struct TokenValidator {
    validator: Validator,
}

impl TokenValidator {
    pub async fn new(secret: &[u8]) -> Self {
        let crypto = worker::crypto();

        let validator = jwt_webcrypto::Validator::new(crypto, Algorithm::HS256, secret)
            .await
            .unwrap();
        Self { validator }
    }

    pub async fn validate_token(&self, token: &str) -> Result<Claims, ValidationError> {
        let payload = self
            .validator
            .validate(token)
            .await
            .map_err(|_| ValidationError)?;

        let claims: Claims = serde_json::from_slice(&payload).map_err(|_| ValidationError)?;
        Ok(claims)
    }
}
