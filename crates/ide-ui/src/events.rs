//! Bridge between backend events and the GPUI main thread.
//!
//! Backend crates emit through [`ide_core::EventSink`], which is
//! `Send + Sync` and must not touch UI state. The bridge forwards every event
//! into an unbounded channel; the workbench drains it in a foreground task and
//! applies the events on the GPUI thread. This replaces Tauri's
//! `emit` / `listen` pair with an in-process channel, no serialization.

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use ide_core::{EventSink, IdeEvent, SharedSink};
use std::sync::Arc;

/// Cloneable sink handed to `ide-host` for every window.
#[derive(Clone)]
pub struct EventBridge {
    tx: UnboundedSender<IdeEvent>,
}

impl EventSink for EventBridge {
    fn emit(&self, event: IdeEvent) {
        // Dropping the receiver (window closed) is not an error.
        let _ = self.tx.unbounded_send(event);
    }
}

impl EventBridge {
    /// A `SharedSink` for code paths that ask for one directly.
    pub fn shared(&self) -> SharedSink {
        Arc::new(self.clone())
    }
}

/// The two halves of the bridge; the receiver is drained by the workbench.
pub struct EventStream {
    pub bridge: EventBridge,
    pub receiver: UnboundedReceiver<IdeEvent>,
}

pub fn stream() -> EventStream {
    let (tx, receiver) = unbounded();
    EventStream { bridge: EventBridge { tx }, receiver }
}
