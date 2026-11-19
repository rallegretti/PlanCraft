//! A plain connection to a server, for PlanCraft (phase 3) and the tests: connect, sign in, open
//! a plan, then send requests and read what the server sends.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::protocol::{ClientId, ClientMsg, Known, MAX_SERVER_FRAME, Opened, PROTOCOL, PlanInfo, Secret, ServerMsg};
use crate::wire::{self, Fingerprint, Trust, WireError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Wire(#[from] WireError),
    /// Refused for good: trying again will not help.
    #[error("{0}")]
    Refused(String),
    /// Refused for now (the server is busy, starting or stopping): it may work later.
    #[error("{0}")]
    Temporary(String),
    /// Signing in failed: `credentials` says the name or password was wrong; otherwise it may
    /// work after `retry_after_secs`.
    #[error("{message}")]
    LoginFailed { message: String, retry_after_secs: u64, credentials: bool },
    #[error("the server sent something unexpected: {0}")]
    Unexpected(String),
}

pub struct Client {
    sender: wire::Sender,
    receiver: wire::Receiver,
    /// The server's key fingerprint (pin it after the person confirmed it).
    pub fingerprint: Fingerprint,
    /// The server's name.
    pub server: String,
}

fn unexpected(msg: &ServerMsg) -> ClientError {
    match msg {
        ServerMsg::Error { message, for_good: true } | ServerMsg::Closing { message, for_good: true } => {
            ClientError::Refused(message.clone())
        }
        ServerMsg::Error { message, .. } | ServerMsg::Closing { message, .. } => ClientError::Temporary(message.clone()),
        ServerMsg::LoginFailed { message, retry_after_secs, credentials } => {
            ClientError::LoginFailed { message: message.clone(), retry_after_secs: *retry_after_secs, credentials: *credentials }
        }
        other => ClientError::Unexpected(format!("{other:?}").chars().take(200).collect()),
    }
}

impl Client {
    /// Connects, checks the server's key against `trust` and says hello.
    pub fn connect(addr: impl ToSocketAddrs, trust: &Trust, app: &str) -> Result<Client, ClientError> {
        let addrs: Vec<SocketAddr> = addr.to_socket_addrs().map_err(WireError::Io)?.collect();
        let mut last = None;
        let mut stream = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let stream = stream.ok_or_else(|| WireError::Io(last.unwrap_or_else(|| std::io::Error::other("no address to connect to"))))?;
        stream.set_read_timeout(Some(CONNECT_TIMEOUT)).map_err(WireError::Io)?;
        let (sender, receiver, fingerprint) = wire::connect(stream, trust)?;
        let mut client = Client { sender, receiver, fingerprint, server: String::new() };
        match client.request(&ClientMsg::Hello { protocol: PROTOCOL, app: app.into() })? {
            ServerMsg::Welcome { server, .. } => client.server = server,
            other => return Err(unexpected(&other)),
        }
        Ok(client)
    }

    /// The two halves of the connection, for a reader and a writer on different threads.
    pub fn into_parts(self) -> (wire::Sender, wire::Receiver) {
        (self.sender, self.receiver)
    }

    pub fn send(&mut self, msg: &ClientMsg) -> Result<(), ClientError> {
        let bytes = serde_json::to_vec(msg).map_err(|e| ClientError::Unexpected(e.to_string()))?;
        self.sender.send(&bytes)?;
        Ok(())
    }

    /// Sends raw bytes as one frame (for tests of hostile input).
    pub fn send_raw(&mut self, bytes: &[u8]) -> Result<(), ClientError> {
        self.sender.send(bytes)?;
        Ok(())
    }

    /// The next message from the server.
    pub fn recv(&mut self) -> Result<ServerMsg, ClientError> {
        let bytes = self.receiver.recv(MAX_SERVER_FRAME)?;
        serde_json::from_slice(&bytes).map_err(|e| ClientError::Unexpected(e.to_string()))
    }

    pub fn set_read_timeout(&self, t: Option<Duration>) -> Result<(), ClientError> {
        self.receiver.set_read_timeout(t).map_err(|e| ClientError::Wire(WireError::Io(e)))
    }

    /// Sends a request and reads the next message.
    pub fn request(&mut self, msg: &ClientMsg) -> Result<ServerMsg, ClientError> {
        self.send(msg)?;
        self.recv()
    }

    pub fn login(&mut self, user: &str, password: &str) -> Result<String, ClientError> {
        match self.request(&ClientMsg::Login { user: user.into(), password: Secret(password.into()) })? {
            ServerMsg::LoggedIn { user } => {
                // Signed in: the server allows two minutes between messages from now on.
                self.set_read_timeout(None)?;
                Ok(user)
            }
            other => Err(unexpected(&other)),
        }
    }

    pub fn list(&mut self) -> Result<Vec<PlanInfo>, ClientError> {
        match self.request(&ClientMsg::List)? {
            ServerMsg::Plans { plans } => Ok(plans),
            other => Err(unexpected(&other)),
        }
    }

    pub fn open(
        &mut self,
        plan: &str,
        client: ClientId,
        known: Option<Known>,
        in_flight: Option<plancraft_core::sync::ChangeId>,
    ) -> Result<Opened, ClientError> {
        match self.request(&ClientMsg::Open { plan: plan.into(), client, known, in_flight })? {
            ServerMsg::Opened(opened) => Ok(*opened),
            other => Err(unexpected(&other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_refusals_for_good_are_final() {
        let gone = ServerMsg::Error { message: "there is no plan 'X' that you may open".into(), for_good: true };
        assert!(matches!(unexpected(&gone), ClientError::Refused(_)));
        let busy = ServerMsg::Error { message: "the server is shutting down".into(), for_good: false };
        assert!(matches!(unexpected(&busy), ClientError::Temporary(_)));
        let closing = ServerMsg::Closing { message: "the plan was closed".into(), for_good: false };
        assert!(matches!(unexpected(&closing), ClientError::Temporary(_)));
        let wrong = ServerMsg::LoginFailed { message: "wrong user name or password".into(), retry_after_secs: 0, credentials: true };
        assert!(matches!(unexpected(&wrong), ClientError::LoginFailed { credentials: true, .. }));
    }
}
