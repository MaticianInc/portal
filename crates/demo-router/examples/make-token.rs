use std::time::{Duration, SystemTime};

use clap::Parser;

use jsonwebtoken::{EncodingKey, Header};
use matic_portal_types::{JwtClaims, Role};

#[derive(Debug, Parser)]
struct Arguments {
    /// The subject name
    subject: String,

    /// The subject's role
    #[arg(long)]
    role: Role,

    /// The tunnel identifier
    #[arg(long)]
    portal_id: u64,

    /// Tunnel service auth token
    #[arg(long)]
    secret: String,

    /// Token lifetime in days
    #[arg(long)]
    days: u64,
}

fn main() {
    let args = Arguments::parse();

    let token = make_token(&args).expect("failed to create token");

    println!("PORTAL_TOKEN={token}");
}

fn make_expiration(lifetime: Duration) -> usize {
    let now = SystemTime::now() + lifetime;
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

fn make_token(args: &Arguments) -> Option<String> {
    let lifetime = Duration::from_secs(60 * 60 * 24 * args.days);

    let claims = JwtClaims {
        sub: args.subject.clone(),
        exp: make_expiration(lifetime),
        role: args.role,
        portal_id: args.portal_id.into(),
    };

    let encoding_key = EncodingKey::from_secret(args.secret.as_bytes());

    let token = jsonwebtoken::encode(&Header::default(), &claims, &encoding_key).ok()?;
    Some(token)
}
