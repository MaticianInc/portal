//! Tests that can be run in a web browser
//!
//! Run with e.g. `wasm-pack test --headless --firefox`

use jwt_webcrypto::{Algorithm, ValidateFailure, Validator};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::Crypto;

wasm_bindgen_test_configure!(run_in_browser);

fn crypto() -> Crypto {
    web_sys::window()
        .expect("failed to get window")
        .crypto()
        .expect("failed to get crypto")
}

#[wasm_bindgen_test]
async fn test_hs256() {
    let validator = jwt_webcrypto::Validator::new(crypto(), Algorithm::HS256, b"s33kr1t")
        .await
        .unwrap();

    let input = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0IiwibmFtZSI6Ik1lIiwiaWF0IjoxNzA1NTM5MDIyfQ.e4oCoBDAp2uSh6U8c2AjodLqxPJqMyD7S5rhOhQ3ccQ";
    let payload = validator
        .validate_signature(input)
        .await
        .expect("failed to validate");
    // This token does not have time constraints.
    validator.validate_timestamps(&payload, 0).unwrap();

    let input = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0IiwibmFtZSI6Ik1lIiwibmJmIjoxNzA1NjMxODEwLCJleHAiOjE3MDU2MzE4MjB9.3e_rc99BEMSCoHCpYQsufPWD87BzfttP0gDHfjo3mXI";
    let payload = validator
        .validate_signature(input)
        .await
        .expect("failed to validate");
    // This token does have timestamps; nbf=1705631810, exp=1705631820
    assert_eq!(
        ValidateFailure::NotYetValid,
        validator
            .validate_timestamps(&payload, 1705631809)
            .unwrap_err()
    );
    validator.validate_timestamps(&payload, 1705631811).unwrap();
    validator
        .validate_timestamps(&payload, 1705631821)
        .unwrap_err();
}

#[wasm_bindgen_test]
async fn test_es256() {
    // openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out private.pem
    // openssl ec -in key2.pem -pubout -out public.pem
    let _private_key = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWeOrkDbHI1IfNQmb
SvPITDLxRETBfc5y1hRsd5w+kfGhRANCAARCSdXGEdx/hrvtJLd20fc2L8wweHYu
xbo+I9PeqdpCF0xePqkFOOuIUQdcK8FLVFLUhybp9oVooy3GlyAn9t+N
-----END PRIVATE KEY-----";
    let public_key = "-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEQknVxhHcf4a77SS3dtH3Ni/MMHh2
LsW6PiPT3qnaQhdMXj6pBTjriFEHXCvBS1RS1Icm6faFaKMtxpcgJ/bfjQ==
-----END PUBLIC KEY-----
";
    let key = pem::parse(public_key).unwrap();
    let validator = Validator::new(crypto(), Algorithm::ES256, key.contents())
        .await
        .unwrap();

    let input = "eyJhbGciOiJFUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0IiwibmFtZSI6Ik1lIiwiaWF0IjoxNzA1NTM5MDIyfQ.od5ZKyDKcJ-eb6VPZcMtQRUkpSrxUZyU3E5BupWiSXDt9K6W60SP59lwBDImVCcHKnEw5gykqe0G-r-J6qCwag";
    let payload = validator
        .validate_signature(input)
        .await
        .expect("failed to validate");
    validator.validate_timestamps(&payload, 1705631811).unwrap();
}
