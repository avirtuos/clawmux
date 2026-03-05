//! Human-attention notifications: terminal bell and OS desktop notifications.
//!
//! Provides [`Notifier`], a lightweight struct that rings the terminal bell
//! and (on supported platforms) spawns a fire-and-forget desktop notification
//! process whenever the agent pipeline needs human attention.

use std::io::Write;
use std::time::{Duration, Instant};

/// Debounce interval: suppress repeated notifications within this window.
const DEBOUNCE: Duration = Duration::from_secs(5);

/// Sends terminal bell and desktop notifications when agents need human input.
///
/// Notifications are debounced: a second call within [`DEBOUNCE`] seconds of
/// the last is silently dropped to avoid rapid-fire dings from cascading events
/// (e.g. an error immediately followed by a phase transition).
pub struct Notifier {
    enabled: bool,
    last_notified: Option<Instant>,
}

impl Notifier {
    /// Creates a new `Notifier`.
    ///
    /// When `enabled` is `false`, all [`notify`](Notifier::notify) calls are
    /// no-ops (no bell, no desktop notification).
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            last_notified: None,
        }
    }

    /// Sends a terminal bell and a desktop notification with the given title and body.
    ///
    /// Skips silently if disabled or if called within 5 seconds of the previous
    /// notification.
    ///
    /// # Arguments
    ///
    /// * `title` - Notification title shown in the OS notification centre.
    /// * `body`  - Notification body text.
    pub fn notify(&mut self, title: &str, body: &str) {
        if !self.enabled {
            return;
        }
        if let Some(last) = self.last_notified {
            if last.elapsed() < DEBOUNCE {
                return;
            }
        }
        self.last_notified = Some(Instant::now());

        // Terminal bell -- works in raw mode; the terminal emulator handles it.
        let _ = std::io::stdout().write_all(b"\x07");
        let _ = std::io::stdout().flush();

        // Desktop notification (fire-and-forget; ignore spawn errors).
        Self::spawn_desktop_notification(title, body);
    }

    /// Spawns a platform-specific desktop notification process.
    ///
    /// On Linux uses `notify-send`; on macOS uses `osascript`.
    /// Other platforms receive a bell only.
    fn spawn_desktop_notification(title: &str, body: &str) {
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("notify-send")
                .arg(title)
                .arg(body)
                .spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let script = format!(
                "display notification \"{}\" with title \"{}\"",
                body.replace('"', "\\\""),
                title.replace('"', "\\\""),
            );
            let _ = std::process::Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .spawn();
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // Bell already sent above; no desktop notification on other platforms.
            let _ = (title, body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notifier_disabled_does_not_notify() {
        let mut n = Notifier::new(false);
        // Should not update last_notified when disabled.
        n.notify("ClawMux", "test body");
        assert!(
            n.last_notified.is_none(),
            "last_notified should stay None when disabled"
        );
    }

    #[test]
    fn test_notifier_enabled_updates_last_notified() {
        let mut n = Notifier::new(true);
        n.notify("ClawMux", "test body");
        assert!(
            n.last_notified.is_some(),
            "last_notified should be set after first notify"
        );
    }

    #[test]
    fn test_debounce_suppresses_rapid_calls() {
        let mut n = Notifier::new(true);
        n.notify("ClawMux", "first");
        let after_first = n.last_notified.unwrap();
        // Second call immediately after -- should be suppressed.
        n.notify("ClawMux", "second");
        let after_second = n.last_notified.unwrap();
        assert_eq!(
            after_first, after_second,
            "last_notified should not change for suppressed call"
        );
    }

    #[test]
    fn test_debounce_allows_after_interval() {
        // Manually set last_notified to > DEBOUNCE seconds ago.
        let mut n = Notifier::new(true);
        n.last_notified = Some(Instant::now() - DEBOUNCE - Duration::from_millis(1));
        let stale = n.last_notified.unwrap();
        n.notify("ClawMux", "after interval");
        let updated = n.last_notified.unwrap();
        assert_ne!(
            stale, updated,
            "last_notified should be refreshed after debounce interval"
        );
    }
}
