//! The messages between PlanCraft and PlanCraft Server: one JSON object per frame, tagged by
//! `type`. Frames from the other side are hostile input: their size is capped before they are
//! read, and anything that does not parse is refused.

use std::fmt;

use plancraft_core::ids::{IdCounters, TaskId};
use plancraft_core::limits::MAX_FILE_BYTES;
use plancraft_core::sync::{Change, ChangeId, Conflict, Entry, Version};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The protocol both sides speak. A client speaking another one is turned away.
pub const PROTOCOL: u32 = 1;

/// The port the server listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 47340;

/// Largest frame accepted before signing in.
pub const MAX_LOGIN_FRAME: usize = 64 * 1024;
/// Largest frame a signed-in person may send (a change can carry a plan's worth of new items).
pub const MAX_CLIENT_FRAME: usize = MAX_FILE_BYTES as usize + 1024 * 1024;
/// Largest frame a client accepts from the server (a whole plan, or a batch of changes).
pub const MAX_SERVER_FRAME: usize = 2 * MAX_FILE_BYTES as usize;

/// Identifies one person's copy of a plan across reconnections (a new one per opened plan).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientId(pub Uuid);

impl ClientId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ClientId {
    fn default() -> Self {
        Self::new()
    }
}

/// A password: never shown in debug output or logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"***\"")
    }
}

/// What someone may do with a plan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// No access: the plan is not listed and cannot be opened.
    #[default]
    None,
    /// Opens the plan and sees changes as they come in.
    Viewer,
    /// Also changes it.
    Editor,
    /// Also manages people and their access (from the app, in a later phase).
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Role::None),
            "viewer" => Some(Role::Viewer),
            "editor" => Some(Role::Editor),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Role::None => "none",
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Admin => "admin",
        }
    }

    pub fn can_edit(self) -> bool {
        self >= Role::Editor
    }
}

/// The plan version a client already has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    /// Which run of the shared plan the version counts in (see [`Snapshot::epoch`]).
    pub epoch: Uuid,
    pub version: Version,
}

/// A request from PlanCraft.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// The first frame: which protocol the client speaks.
    Hello {
        protocol: u32,
        /// The program, e.g. "PlanCraft 0.6.0" (for the server's log).
        #[serde(default)]
        app: String,
    },
    Login {
        user: String,
        password: Secret,
    },
    /// The plans this person may open.
    List,
    /// Opens a plan; the connection then serves that plan only.
    Open {
        plan: String,
        client: ClientId,
        /// The version this copy already has (after a reconnection): the server sends only the
        /// changes since, when it still has them.
        #[serde(default)]
        known: Option<Known>,
        /// A change sent before the connection dropped, whose reply never came.
        #[serde(default)]
        in_flight: Option<ChangeId>,
    },
    Change {
        change: Change,
    },
    /// Asks for another block of new ids.
    Lease,
    /// Asks for the whole plan again.
    Resync,
    /// The task this person is working on (shown to the others).
    Presence {
        #[serde(default)]
        task: Option<TaskId>,
    },
    Ping,
}

/// A plan the server offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanInfo {
    /// The file name in the shared folder, e.g. "Office move.plancraft".
    pub name: String,
    pub role: Role,
    /// How many people have it open.
    pub people: usize,
}

/// Someone who has the plan open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub user: String,
    /// The task they are working on.
    #[serde(default)]
    pub task: Option<TaskId>,
}

/// The whole plan, as the server has it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Changes when the server starts the plan afresh (its file was changed outside the server),
    /// so a version from before never matches one from after.
    pub epoch: Uuid,
    pub version: Version,
    /// The plan as saved in a file (read it like one: `plancraft_core::io::from_json_with_notes`).
    pub plan: String,
}

/// How an opened plan reaches the client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatchUp {
    /// The changes since the version the client knew.
    Entries {
        entries: Vec<Entry>,
    },
    Snapshot {
        snapshot: Snapshot,
    },
}

/// The reply to [`ClientMsg::Open`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Opened {
    pub plan: String,
    pub role: Role,
    pub catch_up: CatchUp,
    /// The first block of new ids (editors only).
    #[serde(default)]
    pub lease: Option<IdCounters>,
    /// Whether the change named in `in_flight` went in.
    #[serde(default)]
    pub in_flight_applied: bool,
    pub people: Vec<Person>,
}

/// A message from the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        protocol: u32,
        /// The server's name (its computer's name).
        server: String,
    },
    LoggedIn {
        user: String,
    },
    LoginFailed {
        message: String,
        /// Seconds to wait before trying again (after several failures).
        #[serde(default)]
        retry_after_secs: u64,
        /// The user name or password was wrong (not a wait or a problem on the server): trying
        /// again with the same ones is pointless.
        #[serde(default)]
        credentials: bool,
    },
    Plans {
        plans: Vec<PlanInfo>,
    },
    Opened(Box<Opened>),
    /// A change the server accepted, in order: everyone gets it, its author included.
    Entry {
        entry: Entry,
    },
    /// A change that was not applied: it clashed (`conflicts`) or was refused (`reason`).
    Rejected {
        change: ChangeId,
        #[serde(default)]
        conflicts: Vec<Conflict>,
        #[serde(default)]
        reason: Option<String>,
        /// The change may go in if sent again later (the server cannot save the plan at the
        /// moment): keep it.
        #[serde(default)]
        retry: bool,
        /// This person's role now, when it is why the change was refused (no longer an editor).
        #[serde(default)]
        role: Option<Role>,
    },
    /// The whole plan, after [`ClientMsg::Resync`] or a change whose base the server no longer
    /// has. `in_flight_applied` says whether the change just sent went in already.
    Snapshot {
        snapshot: Snapshot,
        #[serde(default)]
        in_flight_applied: bool,
    },
    Leased {
        ids: IdCounters,
    },
    People {
        people: Vec<Person>,
    },
    Pong,
    /// A request was refused (not signed in, not allowed, not understood).
    Error {
        message: String,
        /// Trying again will not help (no access, signed out for good, another protocol);
        /// otherwise it may work later (the server is busy, starting or stopping).
        #[serde(default)]
        for_good: bool,
    },
    /// The server is closing the connection (shutting down, or the plan could not be saved).
    Closing {
        message: String,
        /// As for [`ServerMsg::Error`]: this person may not come back.
        #[serde(default)]
        for_good: bool,
    },
}
