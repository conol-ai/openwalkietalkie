//! The two transports of `Client` (contract §3.5): one JSON value per
//! `\n` on the Unix socket, one per text frame on `GET /v1/ws`. Both are
//! reduced to the same pair of a line stream and a line sink so the
//! session logic in `lib.rs` never knows which door it speaks through.
use crate::{ClientError, Transport};
use futures_util::{Sink, Stream};
use std::pin::Pin;

/// Lines from the daemon (`Err` ends the stream).
pub type LineReader = Pin<Box<dyn Stream<Item = Result<String, ClientError>> + Send>>;
/// Lines to the daemon; each `send` writes and flushes one line.
pub type LineWriter = Pin<Box<dyn Sink<String, Error = ClientError> + Send>>;

/// One open connection, before any hello.
pub struct Lines {
    pub reader: LineReader,
    pub writer: LineWriter,
}

/// Open the transport (no hello yet).
pub async fn open(transport: &Transport) -> Result<Lines, ClientError> {
    match transport {
        Transport::Uds(path) => Ok(uds::lines(uds::connect(path).await?)),
        Transport::Ws { url, token } => Ok(ws::lines(ws::connect(url, token).await?)),
    }
}

pub mod uds {
    //! Newline-delimited JSON-RPC over the local door: `tokio::net::UnixStream`
    //! on Unix, the daemon's named pipe on Windows (`crate::windows`). The
    //! framing is the same on both.
    use super::{LineReader, LineWriter, Lines};
    use crate::ClientError;
    use std::path::Path;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

    #[cfg(unix)]
    pub type Stream = tokio::net::UnixStream;
    #[cfg(windows)]
    pub type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

    /// The longest line read from the daemon. Not the daemon's own
    /// `conn::LINE_MAX` (which caps what it reads from clients): a hello
    /// snapshot carries every agent's history (up to 512 KiB each), so a few
    /// busy sessions already exceed that.
    pub const LINE_MAX: usize = 16 * 1024 * 1024;

    /// Connect to the socket (no hello yet).
    #[cfg(unix)]
    pub async fn connect(path: &Path) -> Result<Stream, ClientError> {
        Ok(Stream::connect(path).await?)
    }

    /// Open the pipe, served by this user (no hello yet).
    #[cfg(windows)]
    pub async fn connect(path: &Path) -> Result<Stream, ClientError> {
        Ok(crate::windows::connect(path).await?)
    }

    /// The stream as lines: `\n`-terminated, at most `LINE_MAX` bytes (a
    /// longer one is an error that ends the stream), CR stripped.
    pub fn lines<S>(stream: S) -> Lines
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (read, write) = tokio::io::split(stream);
        let reader: LineReader = Box::pin(futures_util::stream::unfold(
            Some(BufReader::new(read)),
            |state| async move {
                let mut reader = state?;
                let mut buf = Vec::new();
                let limit = (LINE_MAX + 1) as u64;
                let n = {
                    let mut limited = (&mut reader).take(limit);
                    match limited.read_until(b'\n', &mut buf).await {
                        Ok(n) => n,
                        Err(e) => return Some((Err(ClientError::Io(e)), None)),
                    }
                };
                if n == 0 {
                    return None;
                }
                if buf.last() != Some(&b'\n') && buf.len() as u64 >= limit {
                    return Some((
                        Err(ClientError::Protocol(format!(
                            "line longer than {LINE_MAX} bytes"
                        ))),
                        None,
                    ));
                }
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                }
                match String::from_utf8(buf) {
                    Ok(line) => Some((Ok(line), Some(reader))),
                    Err(_) => Some((Err(ClientError::Protocol("not utf-8".into())), None)),
                }
            },
        ));
        let writer: LineWriter = Box::pin(futures_util::sink::unfold(
            write,
            |mut write, line: String| async move {
                write.write_all(line.as_bytes()).await?;
                write.write_all(b"\n").await?;
                write.flush().await?;
                Ok::<_, ClientError>(write)
            },
        ));
        Lines { reader, writer }
    }
}

pub mod ws {
    //! JSON text frames over tokio-tungstenite, bearer in the handshake.
    use super::{LineReader, LineWriter, Lines};
    use crate::ClientError;
    use deck_model::ApiError;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
    use tokio_tungstenite::tungstenite::{Error as WsError, Message};
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    /// The longest text frame read from the daemon; see `uds::LINE_MAX`.
    pub const FRAME_MAX: usize = 16 * 1024 * 1024;

    pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

    /// A tungstenite error as ours: I/O stays I/O, a refused handshake is
    /// `Refused` (401 → unauthorized, 403 → forbidden), the rest is protocol.
    pub fn error_of(e: WsError) -> ClientError {
        match e {
            WsError::Io(e) => ClientError::Io(e),
            WsError::Http(response) => match response.status().as_u16() {
                401 => ClientError::Refused(ApiError::unauthorized()),
                403 => ClientError::Refused(ApiError {
                    code: ApiError::FORBIDDEN,
                    message: "forbidden".into(),
                    data: serde_json::json!({"http": 403}),
                }),
                status => ClientError::Protocol(format!("handshake refused: http {status}")),
            },
            WsError::ConnectionClosed | WsError::AlreadyClosed => ClientError::Disconnected,
            other => ClientError::Protocol(other.to_string()),
        }
    }

    /// Connect to `url` with `Authorization: Bearer <token>` (no hello yet).
    /// The token travels in the handshake header only, never in the URL.
    pub async fn connect(url: &str, token: &str) -> Result<Socket, ClientError> {
        let mut request = url.into_client_request().map_err(error_of)?;
        let bearer = format!("Bearer {token}")
            .parse()
            .map_err(|_| ClientError::Protocol("token is not a valid header value".into()))?;
        request.headers_mut().insert(AUTHORIZATION, bearer);
        let (socket, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(error_of)?;
        Ok(socket)
    }

    /// The socket as lines: one text frame per line (binary, ping, pong
    /// frames are skipped; a close frame or an error ends the stream).
    pub fn lines(socket: Socket) -> Lines {
        let (sink, stream) = socket.split();
        let reader: LineReader = Box::pin(stream.filter_map(|item| async move {
            match item {
                Ok(Message::Text(text)) => {
                    if text.len() > FRAME_MAX {
                        Some(Err(ClientError::Protocol(format!(
                            "frame longer than {FRAME_MAX} bytes"
                        ))))
                    } else {
                        Some(Ok(text.as_str().to_string()))
                    }
                }
                Ok(Message::Close(_)) => Some(Err(ClientError::Disconnected)),
                Ok(_) => None,
                Err(e) => Some(Err(error_of(e))),
            }
        }));
        let writer: LineWriter = Box::pin(
            sink.sink_map_err(error_of)
                .with(|line: String| async move { Ok::<_, ClientError>(Message::text(line)) }),
        );
        Lines { reader, writer }
    }
}
