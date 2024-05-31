use demo_router::router::PortalRouter;

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt::init();

    let Ok(auth_secret) = std::env::var("AUTH_SECRET") else {
        println!("Authentication secret missing. Please set AUTH_SECRET");
        return;
    };

    PortalRouter::new(auth_secret).serve().await;
}
