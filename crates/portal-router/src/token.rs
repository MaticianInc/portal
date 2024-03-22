use jwt_webcrypto::{Algorithm, Validator};
use portal_types::JwtClaims;

#[derive(Debug, Clone, Copy)]
pub struct ValidationError;

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

    pub async fn validate_token(&self, token: &str) -> Result<JwtClaims, ValidationError> {
        let payload = self
            .validator
            .validate(token)
            .await
            .map_err(|_| ValidationError)?;

        let claims: JwtClaims = serde_json::from_slice(&payload).map_err(|_| ValidationError)?;
        Ok(claims)
    }
}
