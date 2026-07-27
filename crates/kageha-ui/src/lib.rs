use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Renderer IDs and event names registered by the host at initialization.
pub type Registry = BTreeMap<String, Vec<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    Agent,
    User,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentCommand {
    Observe,
    Focus {
        revision: u64,
        component: String,
    },
    Present {
        revision: u64,
        component: String,
        props: BTreeMap<String, Value>,
    },
    Close {
        revision: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostCommand {
    SetOwner {
        revision: u64,
        owner: Owner,
    },
    Dispatch {
        revision: u64,
        event: String,
        payload: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    #[serde(default)]
    pub id: u64,
    pub component: String,
    pub props: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: u64,
    pub owner: Owner,
    pub view: Option<View>,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub focus_revision: u64,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Event {
    pub revision: u64,
    pub view_id: u64,
    pub component: String,
    pub name: String,
    pub payload: Value,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    InvalidRegistry,
    StaleRevision,
    UserHasControl,
    UnknownComponent,
    UnknownEvent,
    NoView,
    PayloadTooLarge,
    RevisionExhausted,
}

pub struct Panel {
    registry: Registry,
    state: Snapshot,
}

impl Panel {
    pub fn new(registry: Registry) -> Result<Self, Error> {
        if registry.is_empty()
            || registry.len() > 128
            || registry.iter().any(|(component, events)| {
                !valid_id(component) || events.len() > 32 || events.iter().any(|id| !valid_id(id))
            })
        {
            return Err(Error::InvalidRegistry);
        }
        Ok(Self {
            registry,
            state: Snapshot {
                revision: 0,
                owner: Owner::Agent,
                view: None,
                focus: None,
                focus_revision: 0,
            },
        })
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.state
    }

    pub fn restore(registry: Registry, state: Snapshot) -> Result<Self, Error> {
        let mut panel = Self::new(registry)?;
        if let Some(view) = &state.view {
            if !panel.registry.contains_key(&view.component) {
                return Err(Error::UnknownComponent);
            }
            check_size(&view.props)?;
        }
        if state.focus.as_ref().is_some_and(|id| !panel.registry.contains_key(id)) {
            return Err(Error::UnknownComponent);
        }
        panel.state = state;
        Ok(panel)
    }

    pub fn apply_agent(&mut self, command: AgentCommand) -> Result<&Snapshot, Error> {
        let (revision, view) = match command {
            AgentCommand::Observe => return Ok(&self.state),
            AgentCommand::Focus { revision, component } => {
                if !self.registry.contains_key(&component) {
                    return Err(Error::UnknownComponent);
                }
                self.require_agent_control(revision)?;
                self.advance()?;
                self.state.focus = Some(component);
                self.state.focus_revision = self.state.revision;
                return Ok(&self.state);
            }
            AgentCommand::Present {
                revision,
                component,
                props,
            } => {
                if !self.registry.contains_key(&component) {
                    return Err(Error::UnknownComponent);
                }
                check_size(&props)?;
                let id = self.state.view.as_ref()
                    .filter(|view| view.component == component)
                    .map_or(self.state.revision.saturating_add(1), |view| view.id);
                (revision, Some(View { id, component, props }))
            }
            AgentCommand::Close { revision } => (revision, None),
        };
        self.require_agent_control(revision)?;
        self.advance()?;
        if let Some(view) = &view {
            self.state.focus = Some(view.component.clone());
            self.state.focus_revision = self.state.revision;
        } else if self.state.focus.as_ref() == self.state.view.as_ref().map(|view| &view.component) {
            self.state.focus = None;
        }
        self.state.view = view;
        Ok(&self.state)
    }

    pub fn apply_host(&mut self, command: HostCommand) -> Result<Option<Event>, Error> {
        match command {
            HostCommand::SetOwner { revision, owner } => {
                self.require_revision(revision)?;
                if self.state.owner != owner {
                    self.advance()?;
                    self.state.owner = owner;
                }
                Ok(None)
            }
            HostCommand::Dispatch { revision, event, payload } => {
                self.require_revision(revision)?;
                let view = self.state.view.as_ref().ok_or(Error::NoView)?;
                if !self.registry[&view.component].contains(&event) {
                    return Err(Error::UnknownEvent);
                }
                check_size(&payload)?;
                let component = view.component.clone();
                let view_id = view.id;
                self.advance()?;
                Ok(Some(Event { revision: self.state.revision, view_id, component, name: event, payload }))
            }
        }
    }

    fn require_revision(&self, revision: u64) -> Result<(), Error> {
        if revision == self.state.revision {
            Ok(())
        } else {
            Err(Error::StaleRevision)
        }
    }

    fn require_agent_control(&self, revision: u64) -> Result<(), Error> {
        self.require_revision(revision)?;
        if self.state.owner == Owner::User {
            return Err(Error::UserHasControl);
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<(), Error> {
        self.state.revision = self.state.revision.checked_add(1).ok_or(Error::RevisionExhausted)?;
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.')
    })
}

fn check_size(value: &impl Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::PayloadTooLarge)?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Error::PayloadTooLarge);
    }
    Ok(())
}

pub mod agents_terminal;
