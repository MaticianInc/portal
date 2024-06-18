//! Utilities for interfacing `TunnelSocket` to `AsyncRead`/`AsyncWrite`.

use std::future::Future;
use std::io::{self, Cursor};
use std::pin::{pin, Pin};
use std::task::{ready, Context, Poll};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, Stream, StreamExt as _};
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::Receiver;
use tokio_util::io::StreamReader;
use tokio_util::sync::PollSender;

use crate::TunnelSocket;

impl TunnelSocket {
    /// Wrap the `TunnelSocket` in a type that implements [`AsyncRead`] and [`AsyncWrite`].
    ///
    /// This must be called from a tokio thread, as it will spawn a background task.
    ///
    pub fn into_io(self) -> TunnelSocketIo {
        let keepalive_period = Duration::from_secs(120);
        let (io, task) = self.into_io_split(keepalive_period);
        tokio::spawn(task);
        io
    }

    /// Wrap the `TunnelSocket` in a type that implements [`AsyncRead`] and [`AsyncWrite`].
    ///
    /// This also returns a future that handles keepalive messages;
    /// it must be spawned or awaited. If you don't want to choose how to spawn this
    /// future, use [`into_io`][TunnelSocket::into_io] instead.
    pub fn into_io_split(
        self,
        keepalive_period: Duration,
    ) -> (TunnelSocketIo, impl Future<Output = ()>) {
        let (sink, stream) = self.split();
        let stream = TunnelSocketWithCursor::new(stream);
        let reader = StreamReader::new(stream);

        let (write_channel, rx) = tokio::sync::mpsc::channel(1);

        let write_channel = PollSender::new(write_channel);

        let io = TunnelSocketIo {
            reader,
            write_channel,
        };
        let fut = write_with_keepalives(sink, rx, keepalive_period);
        (io, fut)
    }
}

/// The type of the inner type that implements AsyncRead + AsyncWrite.
type InnerReader = StreamReader<TunnelSocketWithCursor, Cursor<Vec<u8>>>;

#[pin_project]
pub struct TunnelSocketIo {
    #[pin]
    reader: InnerReader,
    write_channel: PollSender<Vec<u8>>,
}

// A passthrough to the inner AsyncRead impl.
impl AsyncRead for TunnelSocketIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.project().reader.poll_read(cx, buf)
    }
}

// A passthrough to the inner AsyncWrite impl.
impl AsyncWrite for TunnelSocketIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        fn broken_pipe<E>(_: E) -> io::Error {
            io::ErrorKind::BrokenPipe.into()
        }

        let write_channel = self.project().write_channel;
        ready!(write_channel.poll_reserve(cx)).map_err(broken_pipe)?;
        let len = buf.len();
        write_channel.send_item(buf.to_vec()).map_err(broken_pipe)?;
        Poll::Ready(Ok(len))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let mut _self = self.project();
        let write_channel = _self.write_channel;
        write_channel.close();

        // It should be possible to return Ok here, but the way that tokio-tungstenite
        // websockets work, the stream (read) side will continue to poll forever, even
        // though we attempted to close the sink() side.
        // If we return an error here, that will cause tokio::io::copy_bidirectional
        // to immediately return instead of waiting on the stream, so that's what we do.
        Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
    }
}

async fn write_with_keepalives(
    mut sink: SplitSink<TunnelSocket, Vec<u8>>,
    mut chan: Receiver<Vec<u8>>,
    keepalive_period: Duration,
) {
    loop {
        let result = tokio::select! {
            incoming = chan.recv() => Some(incoming),
            _ = tokio::time::sleep(keepalive_period) => None,
        };

        let send_buf = match result {
            Some(None) => {
                // recv() returned None, so the sender was dropped.
                tracing::debug!("tunnelsocket sender was closed");
                chan.close();
                let _ = sink.close().await;
                tracing::debug!("exiting 1");
                return;
            }
            Some(Some(incoming)) => incoming,
            None => Vec::new(),
        };

        if sink.send(send_buf).await.is_err() {
            tracing::debug!("exiting 2");
            return;
        }
    }
}

/// A wrapper type that allows us to use `StreamReader` and `SinkWriter`.
///
/// If we do this with StreamExt::map then we lose the Sink impl.
/// All it does is map the stream item to `Cursor<Vec<u8>>` and the sink type to `&[u8]`.
#[pin_project]
struct TunnelSocketWithCursor {
    #[pin]
    inner: SplitStream<TunnelSocket>,
}

impl TunnelSocketWithCursor {
    fn new(inner: SplitStream<TunnelSocket>) -> Self {
        Self { inner }
    }
}

impl Stream for TunnelSocketWithCursor {
    type Item = io::Result<Cursor<Vec<u8>>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.as_mut()
            .project()
            .inner
            .poll_next(cx)
            .map(|opt| opt.map(|result| result.map(Cursor::new)))
    }
}
