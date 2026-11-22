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
///
/// Every window of the process shares one backend, so the bridge fans each event
/// out to all open windows: each has its own receiver (a project opened in a
/// second window must see its own `DocPatched`, and the first window must keep
/// seeing its own).
#[derive(Clone, Default)]
pub struct EventBridge {
    subscribers: Arc<std::sync::Mutex<Vec<UnboundedSender<IdeEvent>>>>,
}

impl EventSink for EventBridge {
    fn emit(&self, event: IdeEvent) {
        let mut subscribers = match self.subscribers.lock() {
            Ok(subscribers) => subscribers,
            Err(poisoned) => poisoned.into_inner(),
        };
        // A closed window's receiver makes the send fail; drop that subscriber
        // rather than keeping a dead channel forever.
        subscribers.retain(|tx| tx.unbounded_send(event.clone()).is_ok());
    }
}

impl EventBridge {
    /// A `SharedSink` for code paths that ask for one directly.
    pub fn shared(&self) -> SharedSink {
        Arc::new(self.clone())
    }

    /// A receiver for one window's event loop.
    pub fn subscribe(&self) -> UnboundedReceiver<IdeEvent> {
        let (tx, receiver) = unbounded();
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(tx);
        }
        receiver
    }
}

/// The two halves of the bridge; the receiver is drained by the workbench.
pub struct EventStream {
    pub bridge: EventBridge,
    pub receiver: UnboundedReceiver<IdeEvent>,
}

pub fn stream() -> EventStream {
    let bridge = EventBridge::default();
    let receiver = bridge.subscribe();
    EventStream { bridge, receiver }
}
