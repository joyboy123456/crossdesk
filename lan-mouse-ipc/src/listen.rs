use futures::{
    Stream, StreamExt,
    stream::{BoxStream, SelectAll},
};
#[cfg(unix)]
use std::path::PathBuf;
use std::{
    collections::HashMap,
    io::ErrorKind,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, WriteHalf};
use tokio_stream::wrappers::LinesStream;

#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::TcpListener;
#[cfg(windows)]
use tokio::net::TcpStream;

use crate::{FrontendEvent, FrontendRequest, IpcError, IpcListenerCreationError};

type ConnectionId = u64;
type ConnectionLineStream = BoxStream<'static, ConnectionInput>;

#[cfg(unix)]
type ConnectionWriter = WriteHalf<UnixStream>;
#[cfg(windows)]
type ConnectionWriter = WriteHalf<TcpStream>;

enum ConnectionInput {
    Line(ConnectionId, Result<String, std::io::Error>),
    Closed(ConnectionId),
}

#[derive(Default)]
struct ConnectionCapabilities {
    control_events: bool,
}

impl ConnectionCapabilities {
    fn observe(&mut self, request: &FrontendRequest) {
        if matches!(request, FrontendRequest::GetControlMode) {
            self.control_events = true;
        }
    }

    fn accepts(&self, event: &FrontendEvent) -> bool {
        self.control_events
            || !matches!(
                event,
                FrontendEvent::ControlMode(_) | FrontendEvent::ControlState(_)
            )
    }
}

struct FrontendConnection {
    tx: ConnectionWriter,
    capabilities: ConnectionCapabilities,
}

pub struct AsyncFrontendListener {
    #[cfg(windows)]
    listener: TcpListener,
    #[cfg(unix)]
    listener: UnixListener,
    #[cfg(unix)]
    socket_path: PathBuf,
    line_streams: SelectAll<ConnectionLineStream>,
    connections: HashMap<ConnectionId, FrontendConnection>,
    next_connection_id: ConnectionId,
}

impl AsyncFrontendListener {
    pub async fn new() -> Result<Self, IpcListenerCreationError> {
        #[cfg(unix)]
        let (socket_path, listener) = {
            let socket_path = crate::default_socket_path()?;

            log::debug!("remove socket: {socket_path:?}");
            if socket_path.exists() {
                // try to connect to see if some other instance
                // of lan-mouse is already running
                match UnixStream::connect(&socket_path).await {
                    // connected -> lan-mouse is already running
                    Ok(_) => return Err(IpcListenerCreationError::AlreadyRunning),
                    // lan-mouse is not running but a socket was left behind
                    Err(e) => {
                        log::debug!("{socket_path:?}: {e} - removing left behind socket");
                        let _ = std::fs::remove_file(&socket_path);
                    }
                }
            }
            let listener = match UnixListener::bind(&socket_path) {
                Ok(ls) => ls,
                // some other lan-mouse instance has bound the socket in the meantime
                Err(e) if e.kind() == ErrorKind::AddrInUse => {
                    return Err(IpcListenerCreationError::AlreadyRunning);
                }
                Err(e) => return Err(IpcListenerCreationError::Bind(e)),
            };
            (socket_path, listener)
        };

        #[cfg(windows)]
        let listener = match TcpListener::bind((crate::IPC_HOST, 0u16)).await {
            Ok(ls) => {
                // bind to port 0 lets the OS pick a free port that can never
                // fall in a Windows dynamic TCP exclusion range; publish it so
                // the frontend can connect.
                let port = ls
                    .local_addr()
                    .map_err(IpcListenerCreationError::Bind)?
                    .port();
                crate::write_ipc_port(port).map_err(IpcListenerCreationError::Bind)?;
                ls
            }
            // bind to port 0 cannot fail with AddrInUse, but keep the guard so
            // an error is still mapped to a clear variant.
            Err(e) if e.kind() == ErrorKind::AddrInUse => {
                return Err(IpcListenerCreationError::AlreadyRunning);
            }
            Err(e) => return Err(IpcListenerCreationError::Bind(e)),
        };

        let adapter = Self {
            listener,
            #[cfg(unix)]
            socket_path,
            line_streams: SelectAll::new(),
            connections: HashMap::new(),
            next_connection_id: 0,
        };

        Ok(adapter)
    }

    pub async fn broadcast(&mut self, notify: FrontendEvent) {
        // encode event
        let mut json = serde_json::to_string(&notify).unwrap();
        json.push('\n');

        let mut disconnected = Vec::new();
        // TODO do simultaneously
        for (connection_id, connection) in &mut self.connections {
            if !connection.capabilities.accepts(&notify) {
                continue;
            }
            if connection.tx.write_all(json.as_bytes()).await.is_err() {
                disconnected.push(*connection_id);
            }
        }

        for connection_id in disconnected {
            self.connections.remove(&connection_id);
        }
    }
}

#[cfg(unix)]
impl Drop for AsyncFrontendListener {
    fn drop(&mut self) {
        log::debug!("remove socket: {:?}", self.socket_path);
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(windows)]
impl Drop for AsyncFrontendListener {
    fn drop(&mut self) {
        // remove the published IPC port so a stale file never makes the
        // frontend wait on a socket nobody is listening on
        crate::remove_ipc_port();
    }
}

impl Stream for AsyncFrontendListener {
    type Item = Result<FrontendRequest, IpcError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match self.line_streams.poll_next_unpin(cx) {
                Poll::Ready(Some(ConnectionInput::Line(connection_id, line))) => {
                    let request = line
                        .map_err(IpcError::from)
                        .and_then(|line| serde_json::from_str(&line).map_err(IpcError::from));
                    if let Ok(request) = &request {
                        if let Some(connection) = self.connections.get_mut(&connection_id) {
                            connection.capabilities.observe(request);
                        }
                    }
                    return Poll::Ready(Some(request));
                }
                Poll::Ready(Some(ConnectionInput::Closed(connection_id))) => {
                    self.connections.remove(&connection_id);
                }
                Poll::Ready(None) | Poll::Pending => break,
            }
        }

        let mut sync = false;
        while let Poll::Ready(Ok((stream, _))) = self.listener.poll_accept(cx) {
            let connection_id = self.next_connection_id;
            self.next_connection_id = self.next_connection_id.wrapping_add(1);
            let (rx, tx) = tokio::io::split(stream);
            let buf_reader = BufReader::new(rx);
            let lines = buf_reader.lines();
            let lines = LinesStream::new(lines)
                .map(move |line| ConnectionInput::Line(connection_id, line))
                .chain(futures::stream::once(async move {
                    ConnectionInput::Closed(connection_id)
                }))
                .boxed();
            self.line_streams.push(lines);
            self.connections.insert(
                connection_id,
                FrontendConnection {
                    tx,
                    capabilities: ConnectionCapabilities::default(),
                },
            );
            sync = true;
        }
        if sync {
            Poll::Ready(Some(Ok(FrontendRequest::Sync)))
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlMode, ControlState, Status};

    #[test]
    fn control_events_require_an_explicit_capability_request() {
        let mut capabilities = ConnectionCapabilities::default();

        assert!(capabilities.accepts(&FrontendEvent::CaptureStatus(Status::Enabled)));
        assert!(!capabilities.accepts(&FrontendEvent::ControlMode(ControlMode::Bidirectional)));
        assert!(!capabilities.accepts(&FrontendEvent::ControlState(ControlState::Idle)));

        capabilities.observe(&FrontendRequest::Sync);
        assert!(!capabilities.accepts(&FrontendEvent::ControlState(ControlState::Idle)));

        capabilities.observe(&FrontendRequest::GetControlMode);
        assert!(capabilities.accepts(&FrontendEvent::ControlMode(ControlMode::Bidirectional)));
        assert!(capabilities.accepts(&FrontendEvent::ControlState(ControlState::Idle)));
    }

    #[test]
    fn capability_state_is_independent_for_each_connection() {
        let mut old_frontend = ConnectionCapabilities::default();
        let mut new_frontend = ConnectionCapabilities::default();

        new_frontend.observe(&FrontendRequest::GetControlMode);

        let event = FrontendEvent::ControlState(ControlState::ReadyToReceive);
        assert!(!old_frontend.accepts(&event));
        assert!(new_frontend.accepts(&event));

        old_frontend.observe(&FrontendRequest::Enumerate());
        assert!(!old_frontend.accepts(&event));
    }
}
