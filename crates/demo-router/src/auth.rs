use std::ops::Deref;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum_extra::headers::authorization::Bearer;
use axum_extra::headers::Authorization;
use axum_extra::TypedHeader;
use jsonwebtoken::{DecodingKey, Validation};

use matic_portal_types::{JwtClaims, Role, PORTAL_JWT_AUDIENCE, PORTAL_JWT_ISSUER};

/// A wrapper around `JwtClaims` so we can impl additional methods and traits.
pub struct Claims(JwtClaims);

impl Deref for Claims {
    type Target = JwtClaims;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Claims {
    pub fn check(&self, role: Role) -> Result<(), StatusCode> {
        if self.0.role != role {
            tracing::error!("{} does not have role {:?}", self.0.sub, role);
            return Err(StatusCode::UNAUTHORIZED);
        }
        // Reject tokens that carry an unexpected issuer or audience.
        // Tokens without iss/aud are still accepted for backward compatibility;
        // once all token issuers populate these fields, tighten to require them.
        if let Some(ref iss) = self.0.iss {
            if iss != PORTAL_JWT_ISSUER {
                tracing::error!("{} has unexpected issuer {:?}", self.0.sub, iss);
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
        if let Some(ref aud) = self.0.aud {
            if aud != PORTAL_JWT_AUDIENCE {
                tracing::error!("{} has unexpected audience {:?}", self.0.sub, aud);
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
        Ok(())
    }
}

impl<S> FromRequestParts<S> for Claims
where
    S: Sync + Send,
{
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let bearer_auth: TypedHeader<Authorization<Bearer>> =
            TypedHeader::from_request_parts(parts, state)
                .await
                .map_err(|_| StatusCode::UNAUTHORIZED)?;
        let auth = parts.extensions.get::<Arc<Auth>>().ok_or_else(|| {
            // The axum router is misconfigured.
            tracing::error!("Auth extension is missing");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        let token_data = jsonwebtoken::decode::<JwtClaims>(
            bearer_auth.token(),
            &auth.decoding_key,
            &auth.validator,
        )
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
        let claims = token_data.claims;
        tracing::debug!("authorized: {}", claims.sub);
        Ok(Claims(claims))
    }
}

/// Authentication state for an axum router.
///
/// Add this to the axum router by calling:
/// ```
/// # use std::sync::Arc;
/// # use demo_router::auth::{Auth, Claims};
/// # use axum::{Extension, Router, extract::State, routing::get};
/// # let secret = "secret";
///
/// # async fn needs_auth(_claims: Claims, _state: State<()>) {}
///
/// let auth = Arc::new(Auth::new(secret));
///
/// let router = Router::new()
///     .route("/needs_auth", get(needs_auth))
///     .layer(Extension(auth));
/// ```
pub struct Auth {
    decoding_key: DecodingKey,
    validator: Validation,
}

impl Auth {
    /// Create a new instance of the authentication service.
    ///
    /// This service handles the verification of JWT tokens
    /// to be used with the "authorization: Bearer" http header.
    pub fn new(secret: &str) -> Self {
        let decoding_key = DecodingKey::from_secret(secret.as_bytes());

        let mut validator = Validation::default();
        validator.set_required_spec_claims(&["exp"]);
        // Issuer and audience are validated post-decode in `Claims::check` so
        // that legacy tokens without these fields are still accepted.
        validator.validate_aud = false;

        Self {
            validator,
            decoding_key,
        }
    }
}
