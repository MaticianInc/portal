use portal_id::{PortalId, ServiceName};
use token::Claims;
use worker::{console_log, event, Env, Headers, Request, Response, RouteContext, Router};

use crate::token::{Role, TokenValidator};

mod durable;
mod portal_id;
mod token;

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> worker::Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();
    router
        .get_async("/connect/host/:id/:service", tunnel_host)
        .get_async("/connect/client/:id/:service", tunnel_client)
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
async fn check_authorization(
    ctx: &RouteContext<()>,
    req: &Request,
    expected_role: Role,
) -> Result<Claims, Error> {
    if let Some(token) = get_auth_header(req.headers()) {
        if let Ok(jwt_secret) = ctx.secret("jwt_secret") {
            let jwt_secret = jwt_secret.to_string();
            let validator = TokenValidator::new(jwt_secret.as_bytes()).await;
            if let Ok(claims) = validator.validate_token(&token).await {
                //console_log!("got signed token with claims: {claims:?}");
                if claims.role == expected_role {
                    return Ok(claims);
                }
            }
        }
    }
    Err(Error::Unauthorized)
}

/// Extract the parameters from the URL.
fn get_url_params(ctx: &RouteContext<()>) -> Option<(PortalId, ServiceName)> {
    let id = ctx.param("id")?;
    let id = id.parse::<PortalId>().ok()?;
    let service = ctx.param("service")?;
    let service = service.to_owned().try_into().ok()?;
    Some((id, service))
}

/// Render the portal ID and service name as a single string, to serve as a unique identifier.
fn tunnel_identifier(portal_id: PortalId, service_name: &ServiceName) -> String {
    format!("{portal_id}:{service_name}")
}

/// Handle incoming connection by tunnel host.
async fn tunnel_host(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    // Check authorization
    let claims = match check_authorization(&ctx, &req, Role::Host).await {
        Err(e) => return e.into_response(),
        Ok(claims) => claims,
    };
    // Extract the URL parameters.
    let Some((portal_id, service_name)) = get_url_params(&ctx) else {
        return Error::MalformedRequest.into_response();
    };
    // Verify that the token allows access to this portal id.
    if claims.portal_id != portal_id {
        console_log!("host {} incorrect portal id", claims.sub);
        return Error::Unauthorized.into_response();
    }

    console_log!("host connect to {portal_id}:{service_name}");

    let namespace = ctx.durable_object("TUNNEL")?;

    let id = namespace.id_from_name(&tunnel_identifier(portal_id, &service_name))?;
    let stub = id.get_stub()?;
    stub.fetch_with_request(req).await
}

/// Handle incoming connection by tunnel client.
async fn tunnel_client(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    // Check authorization
    let claims = match check_authorization(&ctx, &req, Role::Client).await {
        Err(e) => return e.into_response(),
        Ok(claims) => claims,
    };
    // Extract the URL parameters.
    let Some((portal_id, service_name)) = get_url_params(&ctx) else {
        return Error::MalformedRequest.into_response();
    };
    // Verify that the token allows access to this portal id.
    if claims.portal_id != portal_id {
        console_log!("client {} incorrect portal id", claims.sub);
        return Error::Unauthorized.into_response();
    }

    console_log!("client connect to {portal_id}:{service_name}");

    let namespace = ctx.durable_object("TUNNEL")?;
    let id = namespace.id_from_name(&tunnel_identifier(portal_id, &service_name))?;
    let stub = id.get_stub()?;
    stub.fetch_with_request(req).await
}
