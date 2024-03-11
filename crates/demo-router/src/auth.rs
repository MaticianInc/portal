use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum_extra::headers::authorization::Bearer;
use axum_extra::headers::Authorization;
use axum_extra::TypedHeader;
use jsonwebtoken::{DecodingKey, Validation};
use serde::{Deserialize, Serialize};

use crate::tunnelid::PortalId;

// pub struct Capability {
//     role: Role,
// }

// #[async_trait]
// impl<S> FromRequestParts<S> for Capability
// where
//     S: Sync + Send,
// {
//     type Rejection = StatusCode;

//     async fn from_request_parts(
//         parts: &mut axum::http::request::Parts,
//         state: &S,
//     ) -> Result<Self, Self::Rejection> {
//         let claims = JwtClaims::from_request_parts(parts, state).await?;
//         Ok(Self { role: claims.role })
//     }
// }

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

impl JwtClaims {
    pub fn check(&self, role: Role, portal_id: PortalId) -> Result<(), StatusCode> {
        if self.role != role {
            tracing::error!("{} does not have role {:?}", self.sub, role);
            return Err(StatusCode::UNAUTHORIZED);
        }
        if self.portal_id != portal_id {
            tracing::error!(
                "client {} does not have permission to access tunnel {portal_id}",
                self.sub
            );
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

#[async_trait]
impl<S> FromRequestParts<S> for JwtClaims
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
        Ok(claims)
    }
}

/// Authentication state for an axum router.
///
/// Add this to the axum router by calling:
/// ```
/// let auth = Arc::new(Auth::new(secret));
/// // Do this after adding routes that need authorization.
/// router.layer(Extension(auth))
/// ```
pub struct Auth {
    decoding_key: DecodingKey,
    validator: Validation,
}

impl Auth {
    /// Create a new instance of the authentication service.
    ///
    /// This service handles the creation and verification of JWT tokens
    /// to be used with the "authorization: Bearer" http header.
    ///
    /// It also implements authentication from a client certificate (for bots)
    /// and will (TODO) implement openid authentication for humans.
    ///
    pub fn new(secret: &str) -> Self {
        let decoding_key = DecodingKey::from_secret(secret.as_bytes());

        let mut validator = Validation::default();
        // FIXME: add "iss" (issuer) and "aud" (audience) claims.
        validator.set_required_spec_claims(&["exp"]);

        Self {
            validator,
            decoding_key,
        }
    }
}
