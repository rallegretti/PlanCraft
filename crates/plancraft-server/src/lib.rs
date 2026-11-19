//! PlanCraft Server: shares a folder of plans so several people can edit them at once (see
//! docs/COLLABORATION.md).
//!
//! Nothing here runs unless someone starts a server (`plancraft-server FOLDER` or
//! `plancraft serve FOLDER`): PlanCraft itself never listens on the network.
//!
//! - [`wire`]: the encrypted connection (Noise, pure Rust) and the server's key.
//! - [`protocol`]: the messages.
//! - [`users`]: sign-ins (Argon2id hashes, roles, slowing down guessing).
//! - [`journal`] and [`plan`]: one shared plan, kept safe on disk.
//! - [`server`]: connections and requests; [`client`]: the other end.
//! - [`sharing`]: a server run as a separate program for the app's *Share Folder*.
//! - [`discovery`]: announcing a running server on the office network, and finding servers.

pub mod cli;
pub mod client;
pub mod discovery;
pub mod journal;
pub mod link;
pub mod plan;
pub mod protocol;
pub mod server;
pub mod sharing;
pub mod users;
pub mod wire;

/// Where the server writes what it does (one line at a time).
pub type Log = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// A log that writes nothing (tests).
pub fn quiet() -> Log {
    std::sync::Arc::new(|_: &str| {})
}
