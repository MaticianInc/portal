use js_sys::wasm_bindgen::{JsCast, JsValue};
use js_sys::Array;
use types::{Header, ValidatedClaims};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Crypto, CryptoKey, EcKeyImportParams, EcdsaParams, HmacImportParams};

mod types;
pub use types::Algorithm;

// FIXME: just for debugging
#[allow(unused_macros)]
macro_rules! console_log {
    ( $( $t:tt )* ) => {
        web_sys::console::log_1(&format!( $( $t )* ).into());
    }
}

macro_rules! console_error {
    ( $( $t:tt )* ) => {
        web_sys::console::error_1(&format!( $( $t )* ).into());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidateFailure {
    MalformedToken,
    Invalid,
    Expired,
    NotYetValid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupError {
    UnsupportedAlgorithm,
    InvalidKey,
    MalformedKey,
}

pub struct Validator {
    crypto: Crypto,
    pub algorithm: Algorithm,
    key: CryptoKey,
}

impl Validator {
    /// Create a new JWT `Validator` instance.
    ///
    /// The `Crypto` parameter should be retrieved from the Webassembly global
    /// environment; how this happens may depend on which environment you are
    /// using:
    /// - web browser: `Window::crypto()`
    ///
    /// Note about ES256 keys:
    /// - `openssl pkey` PEM and DER outputs are in a format that the Web Cryptography
    ///   API calls `spki`. Other formats exist, but they are so far untested.
    ///
    pub async fn new(crypto: Crypto, algorithm: Algorithm, key: &[u8]) -> Result<Self, SetupError> {
        let subtle = crypto.subtle();
        let key = js_sys::Uint8Array::from(key);
        let import_promise = match algorithm {
            Algorithm::HS256 => {
                let algorithm = HmacImportParams::new("HMAC", &"SHA-256".into());
                subtle.import_key_with_object(
                    "raw",
                    &key,
                    &algorithm,
                    false,
                    &Array::of1(&"verify".into()),
                )
            }
            Algorithm::ES256 => {
                let mut algorithm = EcKeyImportParams::new("ECDSA");
                algorithm.named_curve("P-256");
                subtle.import_key_with_object(
                    "spki",
                    &key,
                    &algorithm,
                    false,
                    &Array::of1(&"verify".into()),
                )
            }
        };
        let promise = import_promise.map_err(|_| SetupError::InvalidKey)?;

        let key = JsFuture::from(promise).await.map_err(|e| {
            console_error!("failed to import key: {e:?}");
            SetupError::InvalidKey
        })?;
        let key: CryptoKey = key.dyn_into().expect("key cast failed");

        Ok(Self {
            crypto,
            algorithm,
            key,
        })
    }

    /// Validate the `exp` and `nbf` fields of a token payload.
    pub fn validate_timestamps(
        &self,
        payload: &[u8],
        current_time: u64,
    ) -> Result<(), ValidateFailure> {
        let claims: ValidatedClaims =
            serde_json::from_slice(payload).map_err(|_| ValidateFailure::MalformedToken)?;
        if let Some(exp) = claims.exp {
            if current_time > exp {
                return Err(ValidateFailure::Expired);
            }
        }
        if let Some(nbf) = claims.nbf {
            if current_time < nbf {
                return Err(ValidateFailure::NotYetValid);
            }
        }
        Ok(())
    }

    /// Validate the token.
    ///
    /// This validates the signature and `exp` and `nbf` timestamps of the token, if present.
    ///
    /// It returns the raw bytes of the token payload; the payload should be deserialized
    /// by the caller to extract the claims.
    pub async fn validate(&self, token: &str) -> Result<Vec<u8>, ValidateFailure> {
        let payload = self.validate_signature(token).await?;
        // Validate that the timestamp is currently valid.
        let current_ts = (js_sys::Date::new_0().get_time() / 1000.0) as u64;
        self.validate_timestamps(&payload, current_ts)?;
        Ok(payload)
    }

    /// Validate the token signature.
    ///
    /// Returns the payload of the JWT.
    ///
    /// Note: this does not validate the `exp` and `nbf` timestamps.
    pub async fn validate_signature(&self, token: &str) -> Result<Vec<u8>, ValidateFailure> {
        let (header_payload, signature) = token
            .rsplit_once('.')
            .ok_or(ValidateFailure::MalformedToken)?;
        let (header, payload) = header_payload
            .split_once('.')
            .ok_or(ValidateFailure::MalformedToken)?;

        let header = b64decode(header.as_bytes()).map_err(|_| ValidateFailure::MalformedToken)?;
        let payload = b64decode(payload.as_bytes()).map_err(|_| ValidateFailure::MalformedToken)?;
        // mutable for no good reason? https://github.com/rustwasm/wasm-bindgen/issues/3795
        let signature =
            b64decode(signature.as_bytes()).map_err(|_| ValidateFailure::MalformedToken)?;

        let message = header_payload.as_bytes().to_owned();

        let alg_obj: js_sys::Object = match self.algorithm {
            Algorithm::HS256 => JsValue::from("HMAC").into(),
            Algorithm::ES256 => EcdsaParams::new("ECDSA", &JsValue::from("SHA-256")).into(),
        };

        let promise = self
            .crypto
            .subtle()
            .verify_with_object_and_u8_array_and_u8_array(&alg_obj, &self.key, &signature, &message)
            .map_err(|_| ValidateFailure::Invalid)?;

        let valid = JsFuture::from(promise)
            .await
            .map_err(|_| ValidateFailure::Invalid)?;
        let valid: js_sys::Boolean = valid.dyn_into().expect("verify cast failed");
        if !valid.value_of() {
            return Err(ValidateFailure::Invalid);
        }

        // Ensure this is a well-formed JWT token.
        let header: Header =
            serde_json::from_slice(&header).map_err(|_| ValidateFailure::MalformedToken)?;
        if header.typ.as_deref() != Some("JWT") {
            return Err(ValidateFailure::Invalid);
        }
        match (header.alg.as_deref(), &self.algorithm) {
            (Some("HS256"), Algorithm::HS256) => (),
            (Some("ES256"), Algorithm::ES256) => (),
            _ => {
                console_log!("header algorithm mismatch");
                return Err(ValidateFailure::Invalid);
            }
        }

        Ok(payload)
    }
}

#[derive(Debug, Clone, Copy)]
struct DecodeFailure;

fn b64decode(input: &[u8]) -> Result<Vec<u8>, DecodeFailure> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.decode(input).map_err(|_| DecodeFailure)
}

pub fn b64encode(input: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.encode(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode() {
        let input = "eyJzdWIiOiIxMjM0IiwibmFtZSI6Ik1lIiwiaWF0IjoxNzA1NTM5MDIyfQ";
        let output = b64decode(input.as_bytes()).unwrap();
        assert_eq!(output, br#"{"sub":"1234","name":"Me","iat":1705539022}"#);
    }
}
