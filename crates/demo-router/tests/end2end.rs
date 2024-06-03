use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

use axum::http::StatusCode;
use demo_router::router::PortalRouter;
use matic_portal_types::{JwtClaims, PortalId, Role};
use tokio_tungstenite::tungstenite::Error as WsError;

fn make_expiration(lifetime: Duration) -> usize {
    let now = SystemTime::now() + lifetime;
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

fn make_token(secret: &str, portal_id: PortalId, role: Role) -> String {
    use jsonwebtoken::{EncodingKey, Header};

    let lifetime = Duration::from_secs(60 * 60 * 24);

    let sub = match role {
        Role::Host => "test_host",
        Role::Client => "test_client",
    }
    .to_owned();

    let claims = JwtClaims {
        sub,
        exp: make_expiration(lifetime),
        role,
        portal_id,
    };

    let encoding_key = EncodingKey::from_secret(secret.as_bytes());

    jsonwebtoken::encode(&Header::default(), &claims, &encoding_key).unwrap()
}

fn url_from_addr(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(addr_v4) => {
            let ip = addr_v4.ip().octets();
            let ip = format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
            let port = addr_v4.port();
            format!("ws://{ip}:{port}")
        }
        SocketAddr::V6(_) => unimplemented!(),
    }
}

#[track_caller]
fn expect_http_error<T>(result: Result<T, WsError>, status: StatusCode) {
    let Err(error) = result else {
        panic!("expected Err, got Ok");
    };
    let WsError::Http(response) = error else {
        panic!("Err was not Http");
    };
    assert_eq!(status, response.status());
}

#[tokio::test]
#[tracing_test::traced_test]
async fn host_auth_fail() {
    let mut router = PortalRouter::new("secret");
    let router_addr = router.bind().await;
    let url = url_from_addr(router_addr);
    tokio::spawn(router.serve());

    let service = matic_portal_client::PortalService::new(&url).unwrap();
    let result = service.tunnel_host("bad token").await;

    expect_http_error(result, StatusCode::UNAUTHORIZED);
}
