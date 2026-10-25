//! The main window fits the screen: it opens maximized, and when it is not
//! maximized it is never larger than the work area of the monitor it is on
//! (the screen without the taskbar), so its title bar and bottom edge stay
//! reachable. The configured size (tauri.conf.json) is the preferred
//! restored size; the window is created hidden and shown once it fits.

use tauri::{Manager, PhysicalSize, Runtime, WebviewWindow, WindowEvent};

/// Fits, centers, maximizes and shows the main window, and keeps a restored
/// window within the work area afterwards.
pub fn open_main_window<R: Runtime>(window: &WebviewWindow<R>) {
    if let Err(err) = fit(window).and_then(|()| window.center()) {
        tracing::warn!("cannot fit the main window to the screen: {err}");
    }
    if let Err(err) = window.maximize() {
        tracing::warn!("cannot maximize the main window: {err}");
    }
    if let Err(err) = window.show() {
        tracing::warn!("cannot show the main window: {err}");
    }
    let tracked = window.clone();
    window.on_window_event(move |event| {
        // Leaving the maximized state, another monitor, another scale: the
        // restored window must still fit.
        if matches!(
            event,
            WindowEvent::Resized(_)
                | WindowEvent::Moved(_)
                | WindowEvent::ScaleFactorChanged { .. }
        ) && let Err(err) = fit(&tracked)
        {
            tracing::debug!("cannot fit the main window to the screen: {err}");
        }
    });
}

/// Shrinks a restored window that is larger than the work area, and the
/// minimum size with it on a small screen.
fn fit<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    if window.is_maximized()? || window.is_fullscreen()? || window.is_minimized()? {
        return Ok(());
    }
    let Some(monitor) = window.current_monitor()? else {
        return Ok(());
    };
    let inner = window.inner_size()?;
    let limit = max_inner_size(monitor.work_area().size, inner, window.outer_size()?);

    if let Some(config) = window
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == window.label())
    {
        let scale = monitor.scale_factor();
        let physical =
            |logical: Option<f64>| logical.map_or(0, |value| (value * scale).round() as u32);
        window.set_min_size(Some(PhysicalSize::new(
            physical(config.min_width).min(limit.width),
            physical(config.min_height).min(limit.height),
        )))?;
    }

    let fitted = PhysicalSize::new(inner.width.min(limit.width), inner.height.min(limit.height));
    if fitted != inner {
        window.set_size(fitted)?;
    }
    Ok(())
}

/// The largest content size whose window fits the work area: the whole
/// frame (title bar and borders) comes off. On Windows 10/11 part of the
/// frame is invisible, so a restored window can end a few pixels short of a
/// maximized one, never beyond it.
fn max_inner_size(
    area: PhysicalSize<u32>,
    inner: PhysicalSize<u32>,
    outer: PhysicalSize<u32>,
) -> PhysicalSize<u32> {
    PhysicalSize::new(
        area.width
            .saturating_sub(outer.width.saturating_sub(inner.width)),
        area.height
            .saturating_sub(outer.height.saturating_sub(inner.height)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restored_window_never_exceeds_the_work_area() {
        // 1366x768 laptop with a 40 px taskbar; 1360x860 content in a
        // Windows 11 frame (8 px borders, 31 px title bar).
        let limit = max_inner_size(
            PhysicalSize::new(1366, 728),
            PhysicalSize::new(1360, 860),
            PhysicalSize::new(1376, 899),
        );
        assert_eq!(limit, PhysicalSize::new(1350, 689));
        // With its frame the window is exactly the work area.
        assert_eq!((limit.width + 16, limit.height + 39), (1366, 728));
    }

    #[test]
    fn frames_without_decorations_use_the_whole_work_area() {
        // Linux with client-side decorations reports no frame.
        let limit = max_inner_size(
            PhysicalSize::new(1920, 1050),
            PhysicalSize::new(1360, 860),
            PhysicalSize::new(1360, 860),
        );
        assert_eq!(limit, PhysicalSize::new(1920, 1050));
    }

    #[test]
    fn a_tiny_work_area_does_not_underflow() {
        let limit = max_inner_size(
            PhysicalSize::new(100, 20),
            PhysicalSize::new(800, 600),
            PhysicalSize::new(816, 639),
        );
        assert_eq!(limit, PhysicalSize::new(84, 0));
    }
}
