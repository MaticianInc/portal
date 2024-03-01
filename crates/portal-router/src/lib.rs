use token::Claims;
use tunnel_id::TunnelId;
use worker::{console_log, event, Env, Headers, Request, Response, RouteContext, Router};

use crate::token::{Role, TokenValidator};

mod durable;
mod token;
mod tunnel_id;

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> worker::Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();
    router
        .get_async("/echo_host/:id", tunnel_host)
        .get_async("/echo_client/:id", tunnel_client)
        .run(req, env)
        .await
}

fn get_auth_header(headers: &Headers) -> Option<String> {
    for (header_name, value) in headers {
        if header_name.eq_ignore_ascii_case("authorization") {
            return value.strip_prefix("Bearer ").map(ToOwned::to_owned);
        }
    }
    None
}

enum Error {
    MalformedRequest,
    Unauthorized,
}

impl Error {
    fn into_response(self) -> worker::Result<Response> {
        match self {
            Error::MalformedRequest => Response::error("Bad Request", 400),
            Error::Unauthorized => Response::error("Unauthorized", 401),
        }
    }
}

/// Verify that an authentication token is present, and it is correct for this request.
///
/// Returns the id of the tunnel.
async fn check_authorization(req: &Request, expected_role: Role) -> Result<Claims, Error> {
    if let Some(token) = get_auth_header(req.headers()) {
        console_log!("XXX auth token: {token}");
        let validator = TokenValidator::new("s33kr1t".as_bytes()).await;
        if let Ok(claims) = validator.validate_token(&token).await {
            console_log!("got signed token with claims: {claims:?}");
            if claims.role == expected_role {
                return Ok(claims);
            }
        }
    }
    return Err(Error::Unauthorized);
}

fn get_tunnel_id(ctx: &RouteContext<()>) -> Option<TunnelId> {
    let id = ctx.param("id")?;
    id.parse::<TunnelId>().ok()
}

/// Handle incoming connection by tunnel host.
async fn tunnel_host(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    // Check authorization
    let claims = match check_authorization(&req, Role::Host).await {
        Err(e) => return e.into_response(),
        Ok(claims) => claims,
    };

    // Get tunnel id and verify it matches auth claims
    let Some(id) = get_tunnel_id(&ctx) else {
        return Error::MalformedRequest.into_response();
    };
    if claims.tunnel_id != id {
        console_log!("host {} incorrect id", claims.sub);
        return Error::Unauthorized.into_response();
    }
    console_log!("host connect to {id}");

    let namespace = ctx.durable_object("TUNNEL")?;
    let id = namespace.id_from_name(&id.to_string())?;
    let stub = id.get_stub()?;
    stub.fetch_with_request(req).await
}

/// Handle incoming connection by tunnel client.
async fn tunnel_client(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    let Some(id) = get_tunnel_id(&ctx) else {
        return Error::MalformedRequest.into_response();
    };
    console_log!("client connect to {id}");

    let namespace = ctx.durable_object("TUNNEL")?;
    let id = namespace.id_from_name(&id.to_string())?;
    let stub = id.get_stub()?;
    stub.fetch_with_request(req).await
}

/// Generate a random channel number.
fn random_channel() -> u32 {
    let mut array = [0u8; 4];
    worker::crypto().get_random_values_with_u8_array(&mut array).unwrap();
    u32::from_ne_bytes(array)
}