use axum::extract::ws::{Message, WebSocket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use std::future::Future;

pub enum Control {
    /// Ask the monitor to yield the websocket.
    Cancel,
    /// Send a message on the websocket.
    Send(Message),
}

pub struct IdleWebSocket {
    name: String,
    control_tx: mpsc::Sender<Control>,
    task: JoinHandle<Result<WebSocket, ()>>,
}

impl IdleWebSocket {
    pub fn new(websocket: WebSocket, name: String, cleanup: impl Future + Send + 'static) -> Self {
        let (control_tx, control_rx) = mpsc::channel(4);
        let task = tokio::spawn(Self::monitor(name.clone(), websocket, control_rx, cleanup));
        Self {
            name,
            control_tx,
            task,
        }
    }

    /// Send a message on the websocket.
    ///
    /// This happens without surrendering ownership of the socket;
    /// calls that want to receive should call `takeover` instead.
    ///
    /// If this fails, the connection may have died.
    pub async fn send(&self, message: Message) -> Result<(), mpsc::error::SendError<Control>> {
        self.control_tx.send(Control::Send(message)).await
    }

    /// Monitor an idle websocket for errors.
    ///
    /// This fn waits on the socket and a oneshot channel that will tell us
    /// when to stop. It will drop any incoming traffic, as the socket is
    /// supposed to be idle except for any Ping messages used as keepalives.
    ///
    /// When we receive a takeover request, we will return the socket.
    pub async fn monitor(
        name: String,
        mut websocket: WebSocket,
        mut control_rx: mpsc::Receiver<Control>,
        cleanup: impl Future,
    ) -> Result<WebSocket, ()> {
        loop {
            let control_ref = &mut control_rx;
            tokio::select! {
                result = websocket.recv() => {
                    match result {
                        Some(Ok(_msg)) => (),
                        _ => {
                            tracing::info!("error on idle websocket {name}");
                            break;
                        }
                    }
                }
                control = control_ref.recv() => {
                    match control {
                        None | Some(Control::Cancel) => break,
                        Some(Control::Send(msg)) => {
                            // If this errors, then presumably the socket is dead.
                            if websocket.send(msg).await.is_err() {
                                break;
                            }
                        }
                    }

                }
            }
        }
        // Run any cleanup tasks (this removes the IdleWebSocket from the global map)
        cleanup.await;

        // Return the websocket so a client can use it.
        Ok(websocket)
    }

    /// Cancel the monitor and return the websocket, if available.
    ///
    /// If this returns an error then the websocket disconnected at some point in the past.
    pub async fn takeover(self) -> Result<WebSocket, TakeoverError> {
        tracing::debug!("cancelling websocket monitor, {}", self.name);
        self.control_tx
            .send(Control::Cancel)
            .await
            .map_err(|_| TakeoverError)?;
        tracing::debug!("waiting for monitor to finish, {}", self.name);
        let result = match self.task.await {
            Err(_) => Err(TakeoverError),
            Ok(Err(_)) => Err(TakeoverError),
            Ok(Ok(websocket)) => Ok(websocket),
        };
        tracing::debug!("monitor {} cancelled, err={}", self.name, result.is_err());
        result
    }
}

/// An error returned by IdleWebSocket::takeover().
#[derive(Debug, thiserror::Error)]
#[error("takeover error")]
pub struct TakeoverError;
