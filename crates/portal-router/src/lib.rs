use worker::{console_log, event, Env, Headers, Request, Response, RouteContext, Router};

use matic_portal_types::{JwtClaims, Role, ServiceName};

use crate::token::TokenValidator;

mod durable;
mod token;

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> worker::Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();
    router
        .get_async("/connect/host_control", host_connect)
        .get_async("/connect/host_accept/:nexus", host_connect)
        .get_async("/connect/client/:service", client_connect)
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
) -> Result<JwtClaims, Error> {
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
fn get_url_params(ctx: &RouteContext<()>) -> Option<ServiceName> {
    let service = ctx.param("service")?;
    let service = service.to_owned().try_into().ok()?;
    Some(service)
}

/// Handle incoming host connections.
///
/// This can handle host control connections as well as host data connections. It
/// performs authentication checks and then forwards the request to the durable
/// object handling this portal id.
///
async fn host_connect(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    // Check authorization
    let claims = match check_authorization(&ctx, &req, Role::Host).await {
        Err(e) => return e.into_response(),
        Ok(claims) => claims,
    };

    let portal_id = claims.portal_id;
    console_log!("host connect to {portal_id}");

    let namespace = ctx.durable_object("TUNNEL")?;

    let id = namespace.id_from_name(&portal_id.to_string())?;
    let stub = id.get_stub()?;
    stub.fetch_with_request(req).await
}

/// Handle incoming connection by tunnel client.
async fn client_connect(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    // Check authorization
    let claims = match check_authorization(&ctx, &req, Role::Client).await {
        Err(e) => return e.into_response(),
        Ok(claims) => claims,
    };
    // Extract the URL parameters.
    let Some(service_name) = get_url_params(&ctx) else {
        return Error::MalformedRequest.into_response();
    };

    let portal_id = claims.portal_id;
    console_log!("client connect to {portal_id}:{service_name}");

    let namespace = ctx.durable_object("TUNNEL")?;
    let id = namespace.id_from_name(&portal_id.to_string())?;
    let stub = id.get_stub()?;
    stub.fetch_with_request(req).await
}
