use axum::extract::ws::WebSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub struct IdleWebSocket {
    name: String,
    cancel_tx: oneshot::Sender<()>,
    task: JoinHandle<Result<WebSocket, ()>>,
}

impl IdleWebSocket {
    pub fn new(websocket: WebSocket, name: String) -> Self {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let task = tokio::spawn(Self::monitor(name.clone(), websocket, cancel_rx));
        Self {
            name,
            cancel_tx,
            task,
        }
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
        mut cancel_rx: oneshot::Receiver<()>,
    ) -> Result<WebSocket, ()> {
        loop {
            let cancel_ref = &mut cancel_rx;
            tokio::select! {
                result = websocket.recv() => {
                    match result {
                        Some(Ok(_msg)) => (),
                        _ => {
                            tracing::info!("error on idle websocket {name}");
                            return Err(());
                        }
                    }
                }
                _ = cancel_ref => break,
            }
        }
        // We received a cancel request; we are returning the websocket so a client can use it.
        Ok(websocket)
    }

    /// Cancel the monitor and return the websocket, if available.
    ///
    /// If this returns an error then the websocket disconnected at some point in the past.
    pub async fn takeover(self) -> Result<WebSocket, ()> {
        tracing::debug!("cancelling websocket monitor, {}", self.name);
        self.cancel_tx.send(())?;
        tracing::debug!("waiting for monitor to finish, {}", self.name);
        let result = match self.task.await {
            Err(_) => Err(()),
            Ok(Err(_)) => Err(()),
            Ok(Ok(websocket)) => Ok(websocket),
        };
        tracing::debug!("monitor {} cancelled, err={}", self.name, result.is_err());
        result
    }
}
