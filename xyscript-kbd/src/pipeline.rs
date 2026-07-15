//! The toolkit-independent input pipeline (§8), shared by every platform hook
//! and by the diagnostic. A platform layer does only OS-specific capture and
//! calls `Pipeline::on_raw` for each native event; the pipeline owns normalize ->
//! resolve -> disposition and tells the hook whether to consume the event.

use crate::event::{KeyEvent, KeyPhase};
use crate::normalize::ScancodeMap;
use crate::resolver::{Disposition, Resolver};

/// What the platform hook should do with the native OS event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// Swallow it: do not let the OS see it (strict default, §6).
    Consume,
    /// Release it to the OS for normal handling (`KF`, §6.1; or an unmapped key
    /// under the prototype's pass-through policy).
    PassThrough,
}

/// Outcome record for diagnostics / dispatch.
pub struct Resolved {
    pub native: u32,
    pub canonical: Option<u8>,
    pub phase: KeyPhase,
    pub bitmask: u32,
    pub disposition: Disposition,
    pub capture: Capture,
}

pub struct Pipeline<M: ScancodeMap> {
    pub resolver: Resolver,
    pub table: M,
    /// If a native code is not in the normalization table, pass it to the OS
    /// rather than swallowing it. Prevents locking the user out of keys the
    /// editor's chart does not (yet) claim. Prototype policy; see README.
    pub unmapped_passthrough: bool,
}

impl<M: ScancodeMap> Pipeline<M> {
    pub fn new(resolver: Resolver, table: M) -> Self {
        Pipeline { resolver, table, unmapped_passthrough: true }
    }

    /// Process one native OS event. Returns the full record (the platform hook
    /// only needs `.capture`; the diagnostic uses the rest).
    pub fn on_raw(&mut self, native: u32, phase: KeyPhase) -> Resolved {
        // 1. Source -> canonical (§8 step 1).
        let canonical = self.table.to_canonical(native);
        let Some(code) = canonical else {
            // Unmapped physical key: prototype passes it through (configurable).
            let capture = if self.unmapped_passthrough {
                Capture::PassThrough
            } else {
                Capture::Consume
            };
            return Resolved {
                native,
                canonical: None,
                phase,
                bitmask: self.resolver.state.current_bitmask(),
                disposition: Disposition::Swallow,
                capture,
            };
        };

        // 2–4. Modifier update / resolve / dispatch-flip-swallow (§8).
        let ev = match phase {
            KeyPhase::Down => KeyEvent::down(code),
            KeyPhase::Up => KeyEvent::up(code),
            KeyPhase::Inject => KeyEvent::inject(code, self.resolver.state.current_bitmask()),
        };
        let bitmask_before = self.resolver.state.current_bitmask();
        let disposition = self.resolver.resolve(ev);
        let capture = match &disposition {
            Disposition::Flip => Capture::PassThrough, // KF releases to OS (§6.1)
            _ => Capture::Consume,                     // dispatch or swallow consume
        };
        Resolved {
            native,
            canonical: Some(code),
            phase,
            bitmask: bitmask_before,
            disposition,
            capture,
        }
    }
}
