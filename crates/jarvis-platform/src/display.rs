use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Graphical session type. Several future features (screen capture, global
/// shortcuts, window management) behave differently per display server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "platform.ts")]
pub enum DisplayServer {
    /// Windows desktop (DWM).
    Windows,
    /// Linux X11 session.
    X11,
    /// Linux Wayland session. Screen capture and input automation require
    /// xdg-desktop-portal and user consent.
    Wayland,
    /// No graphical session was detected (e.g. a TTY or a CI container).
    Headless,
    /// The session type could not be determined.
    Unknown,
}

/// Detects the Linux display server from environment variables.
///
/// `get_env` abstracts environment access so the logic can be tested on any
/// OS. The precedence follows systemd-logind conventions: `XDG_SESSION_TYPE`
/// first, then the presence of `WAYLAND_DISPLAY` / `DISPLAY`.
pub fn detect_linux_display_server<F>(get_env: F) -> DisplayServer
where
    F: Fn(&str) -> Option<String>,
{
    let non_empty = |key: &str| get_env(key).filter(|value| !value.trim().is_empty());

    if let Some(session_type) = non_empty("XDG_SESSION_TYPE") {
        match session_type.trim().to_ascii_lowercase().as_str() {
            "wayland" => return DisplayServer::Wayland,
            "x11" => return DisplayServer::X11,
            "tty" => return DisplayServer::Headless,
            // "unspecified", "mir", ... fall through to the heuristics below.
            _ => {}
        }
    }

    if non_empty("WAYLAND_DISPLAY").is_some() {
        DisplayServer::Wayland
    } else if non_empty("DISPLAY").is_some() {
        DisplayServer::X11
    } else {
        DisplayServer::Headless
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn detect(vars: &[(&str, &str)]) -> DisplayServer {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        detect_linux_display_server(|key| map.get(key).cloned())
    }

    #[test]
    fn session_type_takes_precedence() {
        assert_eq!(
            detect(&[("XDG_SESSION_TYPE", "wayland"), ("DISPLAY", ":0")]),
            DisplayServer::Wayland
        );
        assert_eq!(
            detect(&[
                ("XDG_SESSION_TYPE", "X11"),
                ("WAYLAND_DISPLAY", "wayland-0")
            ]),
            DisplayServer::X11
        );
        assert_eq!(
            detect(&[("XDG_SESSION_TYPE", "tty")]),
            DisplayServer::Headless
        );
    }

    #[test]
    fn falls_back_to_display_variables() {
        assert_eq!(
            detect(&[
                ("XDG_SESSION_TYPE", "unspecified"),
                ("WAYLAND_DISPLAY", "wayland-1")
            ]),
            DisplayServer::Wayland
        );
        // XWayland exposes DISPLAY too, but WAYLAND_DISPLAY wins.
        assert_eq!(
            detect(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]),
            DisplayServer::Wayland
        );
        assert_eq!(detect(&[("DISPLAY", ":1")]), DisplayServer::X11);
    }

    #[test]
    fn empty_values_are_ignored() {
        assert_eq!(
            detect(&[
                ("XDG_SESSION_TYPE", " "),
                ("WAYLAND_DISPLAY", ""),
                ("DISPLAY", "")
            ]),
            DisplayServer::Headless
        );
        assert_eq!(detect(&[]), DisplayServer::Headless);
    }
}
