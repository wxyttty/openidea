//! The backend event bridge: every open window subscribes, so an event reaches
//! all of them and a closed window's subscriber is dropped instead of leaking.

use futures::StreamExt;
use ide_core::{EventSink, IdeEvent};

fn notify(title: &str) -> IdeEvent {
    IdeEvent::Notify { level: ide_core::event::NotifyLevel::Info, title: title.into(), body: None }
}

#[test]
fn one_emit_reaches_every_subscriber() {
    let stream = ide_ui::events::stream();
    let second = stream.bridge.subscribe();
    let third = stream.bridge.subscribe();
    let mut first = stream.receiver;
    let mut second = second;
    let mut third = third;
    stream.bridge.emit(notify("both"));

    let received = |rx: &mut futures::channel::mpsc::UnboundedReceiver<IdeEvent>| {
        futures::executor::block_on(rx.next()).map(|event| match event {
            IdeEvent::Notify { title, .. } => title,
            _ => String::new(),
        })
    };
    assert_eq!(received(&mut first).as_deref(), Some("both"));
    assert_eq!(received(&mut second).as_deref(), Some("both"));
    assert_eq!(received(&mut third).as_deref(), Some("both"), "a window opened later gets a copy");
}

#[test]
fn a_closed_window_is_dropped_and_the_rest_keep_working() {
    let stream = ide_ui::events::stream();
    let mut closed = stream.bridge.subscribe();
    // Dropping the receiver models a closed window.
    closed.close();
    drop(closed);
    let mut live = stream.bridge.subscribe();
    stream.bridge.emit(notify("after close"));
    let event = futures::executor::block_on(live.next());
    assert!(matches!(event, Some(IdeEvent::Notify { .. })), "the live subscriber still receives");
}

#[test]
fn every_subscriber_gets_each_of_several_events_in_order() {
    let stream = ide_ui::events::stream();
    let mut second = stream.bridge.subscribe();
    let mut first = stream.receiver;
    for index in 0..5 {
        stream.bridge.emit(notify(&format!("e{index}")));
    }
    let collect = |rx: &mut futures::channel::mpsc::UnboundedReceiver<IdeEvent>| {
        let mut titles = Vec::new();
        for _ in 0..5 {
            if let Some(IdeEvent::Notify { title, .. }) = futures::executor::block_on(rx.next()) {
                titles.push(title);
            }
        }
        titles
    };
    assert_eq!(collect(&mut first), vec!["e0", "e1", "e2", "e3", "e4"]);
    assert_eq!(collect(&mut second), vec!["e0", "e1", "e2", "e3", "e4"]);
}
