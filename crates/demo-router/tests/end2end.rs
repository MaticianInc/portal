use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::http::StatusCode;
use bytes::Bytes;
use demo_router::router::PortalRouter;
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{EncodingKey, Header};
use matic_portal_client::PortalError;
use matic_portal_types::{JwtClaims, PortalId, Role};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Error as WsError;

enum Expiration {
    /// An expiration date in the future.
    Future(Duration),
    /// An expiration date in the past.
    Past(Duration),
}

impl Expiration {
    /// Create an expiration date in the form expected for a JWT.
    fn as_jwt_seconds(&self) -> usize {
        let now = SystemTime::now();

        let expiration = match *self {
            Self::Future(duration) => now + duration,
            Self::Past(duration) => now - duration,
        };

        expiration
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .try_into()
            .unwrap()
    }
}

/// An expiration date 24 hours in the future.
const EXP_24H: Expiration = Expiration::Future(Duration::from_secs(24 * 60 * 60));
/// Expired 24 hours ago.
const EXPIRED: Expiration = Expiration::Past(Duration::from_secs(24 * 60 * 60));

struct TokenMaker {
    encoding_key: EncodingKey,
}

impl TokenMaker {
    fn new(secret: &[u8]) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret),
        }
    }

    fn make_token(&self, portal_id: PortalId, role: Role, exp: Expiration) -> String {
        let sub = match role {
            Role::Host => "test_host",
            Role::Client => "test_client",
        }
        .to_owned();

        let claims = JwtClaims {
            sub,
            exp: exp.as_jwt_seconds(),
            role,
            portal_id,
        };

        jsonwebtoken::encode(&Header::default(), &claims, &self.encoding_key).unwrap()
    }
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
fn expect_http_error<T>(result: Result<T, PortalError>, status: StatusCode) {
    let Err(error) = result else {
        panic!("expected Err, got Ok");
    };
    let PortalError::Websocket(WsError::Http(response)) = error else {
        panic!("Err was not Http");
    };
    assert_eq!(status, response.status());
}

#[track_caller]
fn expect_timeout_error<T>(result: Result<T, PortalError>) {
    assert!(matches!(result, Err(PortalError::Timeout)));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn host_auth_fail() {
    let mut router = PortalRouter::new("secret");
    let router_addr = router.bind().await;
    let url = url_from_addr(router_addr);
    tokio::spawn(router.serve());

    // A host with an invalid token
    let service = matic_portal_client::PortalService::new(&url).unwrap();
    let result = service.tunnel_host("bad_host_token").await;
    expect_http_error(result, StatusCode::UNAUTHORIZED);

    // A client with an invalid token
    let result = service
        .tunnel_client("bad_client_token", "some_service")
        .await;
    expect_http_error(result, StatusCode::UNAUTHORIZED);

    let tokens = TokenMaker::new(b"secret");
    let host_token = tokens.make_token(PortalId::from(1234), Role::Host, EXP_24H);
    let client_token = tokens.make_token(PortalId::from(1234), Role::Client, EXP_24H);

    // A host trying to use a client token
    let result = service.tunnel_host(&client_token).await;
    expect_http_error(result, StatusCode::UNAUTHORIZED);

    // A client trying to use a host token
    let result = service.tunnel_client(&host_token, "some_service").await;
    expect_http_error(result, StatusCode::UNAUTHORIZED);

    // Verify that the tokens are valid
    let host = service.tunnel_host(&host_token).await.unwrap();
    let result = service.tunnel_client(&client_token, "some_service").await;
    expect_timeout_error(result);
    drop(host);

    // A host trying to use an expired token
    let host_token = tokens.make_token(PortalId::from(1234), Role::Host, EXPIRED);
    let result = service.tunnel_host(&host_token).await;
    expect_http_error(result, StatusCode::UNAUTHORIZED);

    // A client trying to use an expired token
    let client_token = tokens.make_token(PortalId::from(1234), Role::Client, EXPIRED);
    let result = service.tunnel_client(&client_token, "some_service").await;
    expect_http_error(result, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[tracing_test::traced_test]
async fn end2end() {
    let mut router = PortalRouter::new("secret");
    let router_addr = router.bind().await;
    let url = url_from_addr(router_addr);
    tokio::spawn(router.serve());

    let tokens = TokenMaker::new(b"secret");
    let client_token = tokens.make_token(PortalId::from(1234), Role::Client, EXP_24H);
    let service = Arc::new(matic_portal_client::PortalService::new(&url).unwrap());
    let service2 = service.clone();

    let (host_ready_tx, host_ready_rx) = oneshot::channel();

    tokio::spawn(async move {
        let host_token = tokens.make_token(PortalId::from(1234), Role::Host, EXP_24H);
        let mut host = service2.tunnel_host(&host_token).await.unwrap();
        host_ready_tx.send(()).unwrap();
        let incoming = host.next_client().await.unwrap();
        assert_eq!(incoming.service_name(), "hello_world");
        let mut sock = incoming.connect(&host_token).await.unwrap();
        let message = sock.next().await.unwrap().unwrap();
        assert_eq!(message, [11, 22, 33].as_ref());
    });

    host_ready_rx.await.unwrap();

    let mut client = service
        .tunnel_client(&client_token, "hello_world")
        .await
        .unwrap();
    client
        .send(Bytes::from([11, 22, 33].as_ref()))
        .await
        .unwrap();
}

#[tokio::test]
#[tracing_test::traced_test]
async fn end2end_io() {
    let mut router = PortalRouter::new("secret");
    let router_addr = router.bind().await;
    let url = url_from_addr(router_addr);
    tokio::spawn(router.serve());

    let tokens = TokenMaker::new(b"secret");
    let client_token = tokens.make_token(PortalId::from(1234), Role::Client, EXP_24H);
    let service = Arc::new(matic_portal_client::PortalService::new(&url).unwrap());
    let service2 = service.clone();

    let (host_ready_tx, host_ready_rx) = oneshot::channel();

    tokio::spawn(async move {
        let host_token = tokens.make_token(PortalId::from(1234), Role::Host, EXP_24H);
        let mut host = service2.tunnel_host(&host_token).await.unwrap();
        host_ready_tx.send(()).unwrap();
        let incoming = host.next_client().await.unwrap();
        assert_eq!(incoming.service_name(), "hello_world");
        let sock = incoming.connect(&host_token).await.unwrap();

        // Test data send/receive using AsyncRead/AsyncWrite implementation.
        let mut host_io = sock.into_io();
        let mut buffer = [0u8; 5];
        let count = host_io.read_exact(&mut buffer).await.unwrap();
        assert_eq!(count, 5);
        assert_eq!(buffer, [11, 22, 33, 44, 55]);
    });

    host_ready_rx.await.unwrap();

    let client = service
        .tunnel_client(&client_token, "hello_world")
        .await
        .unwrap();

    let mut client_io = client.into_io();

    client_io.write_all(&[11, 22, 33]).await.unwrap();
    client_io.write_all(&[44, 55, 66]).await.unwrap();
}
