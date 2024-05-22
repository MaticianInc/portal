use std::borrow::Cow;
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{anyhow, bail, Context};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use portal_client::PortalService;
use tokio::io::AsyncBufReadExt;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Arguments {
    /// Server base URL
    #[arg(long, default_value = "ws://localhost:3000")]
    server: String,

    /// The service name
    #[arg(long, default_value = "echo")]
    service: String,

    /// Tunnel service auth token
    #[arg(long)]
    auth_token: Option<String>,
}

async fn run_client(args: &Arguments) -> anyhow::Result<()> {
    tracing::info!("running echo client");
    let service = PortalService::new(&args.server)?;
    let mut tunnel = service
        .tunnel_client(
            args.auth_token.as_deref().unwrap_or_default(),
            &args.service,
        )
        .await
        .context("failed to connect to service")?;

    let stdin = tokio::io::stdin();
    let stdin = tokio::io::BufReader::new(stdin);
    let mut stdin_lines = stdin.lines();

    loop {
        tokio::select! {
            stream_result = tunnel.next() => {
                // Using the ? operator here confuses the tokio::select macro
                let message = match stream_result {
                    None => break Err(anyhow!("websocket closed")),
                    Some(Err(e)) => break Err(anyhow!(e)),
                    Some(Ok(message)) => message,
                };

                let printable_message = match std::str::from_utf8(&message) {
                    Ok(text) => {
                        let mut output = Cow::from(text);

                        if let Some(timestamp) = text.strip_prefix("!! ") {
                            let timestamp: Result<f64, _> = timestamp.parse();
                            if let Ok(timestamp ) = timestamp {
                                let delta_time = 1000.0 * (get_timestamp() - timestamp);
                                output = format!("!! round-trip time {delta_time} ms").into();
                            }
                        }
                        output
                    },
                    Err(_) => Cow::from(format!("binary message ({} bytes)", message.len())),
                };
                tracing::info!("message from host: {printable_message}");
            },
            stdin_result = stdin_lines.next_line() => {
                match stdin_result {
                    Ok(Some(mut text)) => {
                        let message = if text.trim().is_empty() {
                            // Send a ping instead of an empty message.
                            Vec::new()
                        } else {
                            // The message "!!" means "measure the round-trip message time".
                            if text.trim() == "!!" {
                                text = format!("!! {}", get_timestamp());
                            }
                            text.into()
                        };
                        // FIXME: use tunnel.feed() instead? See SinkExt documentation.
                        tunnel.send(message).await.unwrap()},
                    _ => bail!("stdin closed"),
                }

            },
        }
    }
}

#[tokio::main]
async fn main() {
    let mut args = Arguments::parse();

    // Allow the auth token to be passed in an environment variable.
    if args.auth_token.is_none() {
        if let Some(env_token) = option_env!("PORTAL_TOKEN") {
            args.auth_token = Some(env_token.to_owned());
        }
    }

    tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();

    let result = run_client(&args).await;
    if let Err(e) = result {
        tracing::info!("connection ended: {e:#}");
    }
}

static TIMESTAMP_BASE: OnceLock<Instant> = OnceLock::new();

/// Get a monotonic timestamp, in f64 seconds.
fn get_timestamp() -> f64 {
    let base = TIMESTAMP_BASE.get_or_init(Instant::now);
    base.elapsed().as_secs_f64()
}
