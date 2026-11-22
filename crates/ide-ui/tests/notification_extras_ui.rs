//! Notification extras: the sound preference reaches the log's prefs, and
//! "Don't show again" suppresses a title for the session.

use gpui_kit::{AppContext, TestAppContext};
use ide_core::event::NotifyLevel;
use ide_ui::notifications::{BalloonDisplay, NotificationLog, NotificationPrefs, Suppressed};

#[gpui_kit::test]
fn the_sound_preference_is_carried_by_the_prefs(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let log = cx.new(|_| NotificationLog::new());
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.set_prefs(NotificationPrefs {
                balloons: true,
                display: BalloonDisplay::Balloon,
                log: true,
                sound: true,
            });
            log.push(NotifyLevel::Info, "build finished", None::<String>);
            cx.notify();
        });
        assert!(log.read(cx).prefs().sound, "the log keeps the sound flag");
    });
}

#[gpui_kit::test]
fn dont_show_again_drops_the_balloon_but_keeps_a_logged_entry(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let log = cx.new(|_| NotificationLog::new());
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.set_prefs(NotificationPrefs {
                balloons: true,
                display: BalloonDisplay::Balloon,
                log: true,
                sound: false,
            });
            log.push(NotifyLevel::Warning, "formatter missing", None::<String>);
            cx.notify();
        });
        assert_eq!(log.read(cx).balloons().len(), 1);
        let title = log.read(cx).items()[0].title.to_string();
        log.update(cx, |log, cx| log.suppress(title.clone(), cx));
        assert!(log.read(cx).balloons().is_empty(), "the balloon is gone");
        assert_eq!(log.read(cx).len(), 1, "the history entry stays");
        assert_eq!(log.read(cx).suppressed_titles(), vec![title]);
    });
}

#[test]
fn the_suppressed_set_answers_contains() {
    let mut suppressed = Suppressed::default();
    assert!(!suppressed.contains("formatter missing"));
    assert!(suppressed.is_empty());
    suppressed.suppress("formatter missing");
    assert!(suppressed.contains("formatter missing"));
    assert_eq!(suppressed.len(), 1);
    // Suppressing twice still holds one title.
    suppressed.suppress("formatter missing");
    assert_eq!(suppressed.len(), 1);
}
