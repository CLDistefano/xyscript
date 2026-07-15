//! Programmatic injection channel for virtual keys (§3.5).
//!
//! A second event source feeding the *same* resolver as the OS normalizer. A
//! poke carries its full `(bitmask, key_code)` pair and is deterministic,
//! independent of whatever the user is physically holding.
//!
//! Because a poke can fire any cell, and cells execute XPL, this channel is
//! effectively remote control of the editor. It is therefore **disabled by
//! default** and gated by access control — a security requirement, not optional
//! polish (§3.5, Appendix A).

use crate::event::{EventSource, KeyEvent};
use crate::model::{Bitmask, KeyCode};
use std::collections::VecDeque;

/// Access policy for the channel. Default is the secure default: disabled.
#[derive(Debug, Clone)]
#[derive(Default)]
pub enum Access {
    /// No injection accepted at all.
    #[default]
    Disabled,
    /// Enabled, but every poke must present a token matching this secret.
    Token(String),
    /// Enabled with an allow-list of permitted client identifiers.
    AllowList(Vec<String>),
}


#[derive(Debug, PartialEq, Eq)]
pub enum InjectError {
    /// The channel is disabled.
    Disabled,
    /// Credentials did not satisfy the access policy.
    Unauthorized,
    /// The key code is outside the reserved virtual band (a poke must target a
    /// virtual cell, never a physical code — that is the non-interference
    /// guarantee, §3.5).
    NotVirtual,
}

/// An in-process injection channel. A real editor would back this with a
/// local-only IPC endpoint; the queue and the access checks are the same.
#[derive(Debug, Default)]
pub struct InjectionChannel {
    access: Access,
    /// First virtual code; pokes must target `>= virtual_base` (§3.5).
    virtual_base: Option<KeyCode>,
    queue: VecDeque<KeyEvent>,
}

impl InjectionChannel {
    /// Construct disabled (the only safe default).
    pub fn new(virtual_base: Option<KeyCode>) -> Self {
        InjectionChannel { access: Access::Disabled, virtual_base, queue: VecDeque::new() }
    }

    /// Explicitly enable with a policy. Until this is called, every poke is
    /// rejected with `InjectError::Disabled`.
    pub fn enable(&mut self, access: Access) {
        self.access = access;
    }

    fn authorized(&self, credential: Option<&str>) -> bool {
        match &self.access {
            Access::Disabled => false,
            Access::Token(secret) => credential == Some(secret.as_str()),
            Access::AllowList(ids) => {
                credential.map(|c| ids.iter().any(|i| i == c)).unwrap_or(false)
            }
        }
    }

    /// Submit a poke. `credential` is the caller's token / client id. The poke is
    /// only queued if the channel is enabled, the credential authorizes, and the
    /// target code is in the virtual band.
    pub fn poke(
        &mut self,
        key_code: KeyCode,
        bitmask: Bitmask,
        credential: Option<&str>,
    ) -> Result<(), InjectError> {
        if matches!(self.access, Access::Disabled) {
            return Err(InjectError::Disabled);
        }
        if !self.authorized(credential) {
            return Err(InjectError::Unauthorized);
        }
        if let Some(vb) = self.virtual_base {
            if key_code < vb {
                return Err(InjectError::NotVirtual);
            }
        } else {
            // No virtual band configured => nothing is injectable.
            return Err(InjectError::NotVirtual);
        }
        self.queue.push_back(KeyEvent::inject(key_code, bitmask));
        Ok(())
    }
}

impl EventSource for InjectionChannel {
    fn next_event(&mut self) -> Option<KeyEvent> {
        self.queue.pop_front()
    }
}
