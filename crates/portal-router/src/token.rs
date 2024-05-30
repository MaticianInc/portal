use jwt_webcrypto::{Algorithm, Validator};
use portal_types::JwtClaims;

#[derive(Debug, Clone, Copy)]
pub struct ValidationError;

pub struct TokenValidator {
    validator: Validator,
}

impl TokenValidator {
    pub async fn new(secret: &[u8]) -> Self {
        let crypto = worker_global::crypto();

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

// Workers don't access the WebCrypto API the same way as browsers.
// So we need to help ourselves via the global environment.
// See also https://github.com/cloudflare/workers-rs/pull/471
pub mod worker_global {
    use wasm_bindgen::JsCast;

    pub fn crypto() -> web_sys::Crypto {
        let global: web_sys::WorkerGlobalScope = js_sys::global().unchecked_into();
        global.crypto().expect("failed to acquire Crypto")
    }
}
