//! X11 capture with x11rb (pure Rust): `GetImage` on the root window,
//! RandR for monitors and EWMH properties for the window list.

use async_trait::async_trait;
use image::RgbaImage;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, ImageFormat, ImageOrder, MapState, Screen, Setup, Window,
};
use x11rb::rust_connection::RustConnection;

use crate::capture::{Captured, ScreenCapture, blocking};
use crate::error::{Result, VisionError};
use crate::types::{CaptureCapabilities, CaptureSupport, DisplayInfo, WindowInfo};

#[derive(Debug)]
pub struct X11Capture;

fn connect() -> Result<(RustConnection, usize)> {
    x11rb::connect(None)
        .map_err(|err| VisionError::Unsupported(format!("cannot connect to the X server: {err}")))
}

fn x11_error(err: impl std::fmt::Display) -> VisionError {
    VisionError::Capture(err.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

struct Monitor {
    name: String,
    rect: Rect,
    primary: bool,
}

fn atom(conn: &RustConnection, name: &str) -> Result<u32> {
    Ok(conn
        .intern_atom(false, name.as_bytes())
        .map_err(x11_error)?
        .reply()
        .map_err(x11_error)?
        .atom)
}

fn monitors(conn: &RustConnection, screen: &Screen) -> Vec<Monitor> {
    let listed = conn
        .randr_get_monitors(screen.root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok());
    let mut found: Vec<Monitor> = listed
        .map(|reply| {
            reply
                .monitors
                .iter()
                .enumerate()
                .map(|(index, m)| Monitor {
                    name: conn
                        .get_atom_name(m.name)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| format!("Display {}", index + 1)),
                    rect: Rect {
                        x: m.x,
                        y: m.y,
                        width: m.width,
                        height: m.height,
                    },
                    primary: m.primary,
                })
                .collect()
        })
        .unwrap_or_default();
    if found.is_empty() {
        // No RandR: the whole screen.
        found.push(Monitor {
            name: "Screen".into(),
            rect: Rect {
                x: 0,
                y: 0,
                width: screen.width_in_pixels,
                height: screen.height_in_pixels,
            },
            primary: true,
        });
    }
    found
}

/// Captures a rectangle of the root window (what is on screen).
fn grab(conn: &RustConnection, setup: &Setup, screen: &Screen, rect: Rect) -> Result<RgbaImage> {
    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            screen.root,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            u32::MAX,
        )
        .map_err(x11_error)?
        .reply()
        .map_err(x11_error)?;
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|d| d.visuals.iter())
        .find(|v| v.visual_id == reply.visual)
        .map(|v| (v.red_mask, v.green_mask, v.blue_mask))
        .unwrap_or((0x00FF_0000, 0x0000_FF00, 0x0000_00FF));
    let format = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == reply.depth)
        .ok_or_else(|| {
            VisionError::Capture(format!("no pixmap format for depth {}", reply.depth))
        })?;
    to_rgba(
        &reply.data,
        Layout {
            width: u32::from(rect.width),
            height: u32::from(rect.height),
            bits_per_pixel: u32::from(format.bits_per_pixel),
            scanline_pad: u32::from(format.scanline_pad),
            lsb_first: setup.image_byte_order == ImageOrder::LSB_FIRST,
            masks: visual,
        },
    )
}

#[derive(Debug, Clone, Copy)]
struct Layout {
    width: u32,
    height: u32,
    bits_per_pixel: u32,
    scanline_pad: u32,
    lsb_first: bool,
    masks: (u32, u32, u32),
}

/// Converts Z-pixmap data (24 or 32 bits per pixel, any channel masks) to RGBA.
fn to_rgba(data: &[u8], layout: Layout) -> Result<RgbaImage> {
    if layout.bits_per_pixel != 24 && layout.bits_per_pixel != 32 {
        return Err(VisionError::Unsupported(format!(
            "{}-bit X11 displays are not supported",
            layout.bits_per_pixel
        )));
    }
    let pad = layout.scanline_pad.max(8) as usize;
    let bits = layout.width as usize * layout.bits_per_pixel as usize;
    let stride = bits.div_ceil(pad) * pad / 8;
    let bytes_per_pixel = layout.bits_per_pixel as usize / 8;
    if data.len() < stride * layout.height as usize {
        return Err(VisionError::Capture(
            "the X server returned too little image data".into(),
        ));
    }
    let (red, green, blue) = layout.masks;
    let mut image = RgbaImage::new(layout.width, layout.height);
    for (y, row) in data.chunks(stride).take(layout.height as usize).enumerate() {
        for x in 0..layout.width as usize {
            let pixel = &row[x * bytes_per_pixel..(x + 1) * bytes_per_pixel];
            let value = if layout.lsb_first {
                pixel
                    .iter()
                    .rev()
                    .fold(0u32, |acc, b| (acc << 8) | u32::from(*b))
            } else {
                pixel.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b))
            };
            image.put_pixel(
                x as u32,
                y as u32,
                image::Rgba([
                    channel(value, red),
                    channel(value, green),
                    channel(value, blue),
                    255,
                ]),
            );
        }
    }
    Ok(image)
}

/// One color channel scaled to 8 bits.
fn channel(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let raw = (value & mask) >> shift;
    let scaled = if bits >= 8 {
        raw >> (bits - 8)
    } else {
        // Repeat the high bits into the low ones (5 bits → 8 bits).
        (raw << (8 - bits)) | (raw >> (2 * bits).saturating_sub(8))
    };
    u8::try_from(scaled.min(255)).unwrap_or(u8::MAX)
}

fn text_property(
    conn: &RustConnection,
    window: Window,
    property: u32,
    kind: u32,
) -> Option<String> {
    let reply = conn
        .get_property(false, window, property, kind, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    (!reply.value.is_empty()).then(|| String::from_utf8_lossy(&reply.value).into_owned())
}

/// Top-level windows the window manager knows, with titles.
fn client_windows(
    conn: &RustConnection,
    screen: &Screen,
) -> Result<Vec<(Window, String, Option<String>)>> {
    let list = atom(conn, "_NET_CLIENT_LIST")?;
    let name = atom(conn, "_NET_WM_NAME")?;
    let utf8 = atom(conn, "UTF8_STRING")?;
    let reply = conn
        .get_property(false, screen.root, list, AtomEnum::WINDOW, 0, 4096)
        .map_err(x11_error)?
        .reply()
        .map_err(x11_error)?;
    let Some(ids) = reply.value32() else {
        return Err(VisionError::Unsupported(
            "the window manager does not publish its window list".into(),
        ));
    };
    let mut windows = Vec::new();
    for window in ids {
        let viewable = conn
            .get_window_attributes(window)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|a| a.map_state == MapState::VIEWABLE);
        if !viewable {
            continue;
        }
        let title = text_property(conn, window, name, utf8)
            .or_else(|| {
                text_property(
                    conn,
                    window,
                    AtomEnum::WM_NAME.into(),
                    AtomEnum::STRING.into(),
                )
            })
            .unwrap_or_default();
        if title.trim().is_empty() {
            continue;
        }
        let app = text_property(
            conn,
            window,
            AtomEnum::WM_CLASS.into(),
            AtomEnum::STRING.into(),
        )
        .and_then(|class| class.split('\0').nth(1).map(str::to_owned))
        .filter(|class| !class.is_empty());
        windows.push((window, title, app));
    }
    Ok(windows)
}

/// The window's area on screen, clipped to the screen.
fn window_rect(conn: &RustConnection, screen: &Screen, window: Window) -> Result<Rect> {
    let geometry = conn
        .get_geometry(window)
        .map_err(x11_error)?
        .reply()
        .map_err(|_| VisionError::NotFound("the window was closed".into()))?;
    let origin = conn
        .translate_coordinates(window, screen.root, 0, 0)
        .map_err(x11_error)?
        .reply()
        .map_err(x11_error)?;
    let left = i32::from(origin.dst_x).max(0);
    let top = i32::from(origin.dst_y).max(0);
    let right = (i32::from(origin.dst_x) + i32::from(geometry.width))
        .min(i32::from(screen.width_in_pixels));
    let bottom = (i32::from(origin.dst_y) + i32::from(geometry.height))
        .min(i32::from(screen.height_in_pixels));
    if right <= left || bottom <= top {
        return Err(VisionError::Capture("the window is off screen".into()));
    }
    let clamp16 = |v: i32| i16::try_from(v).unwrap_or(i16::MAX);
    let clampu16 = |v: i32| u16::try_from(v).unwrap_or(u16::MAX);
    Ok(Rect {
        x: clamp16(left),
        y: clamp16(top),
        width: clampu16(right - left),
        height: clampu16(bottom - top),
    })
}

fn screen_of(conn: &RustConnection, number: usize) -> Result<Screen> {
    conn.setup()
        .roots
        .get(number)
        .cloned()
        .ok_or_else(|| VisionError::Capture("the X server has no such screen".into()))
}

#[async_trait]
impl ScreenCapture for X11Capture {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            screen: CaptureSupport::Available,
            window: CaptureSupport::Available,
            selection: CaptureSupport::Unavailable,
            notes: vec![
                "X11 lets applications read the screen without asking; JARVIS captures only when you press a capture button and shows the result first.".into(),
                "A window capture shows what is visible in the window's area, including windows on top of it.".into(),
            ],
        }
    }

    async fn displays(&self) -> Result<Vec<DisplayInfo>> {
        blocking(|| {
            let (conn, number) = connect()?;
            let screen = screen_of(&conn, number)?;
            Ok(monitors(&conn, &screen)
                .into_iter()
                .enumerate()
                .map(|(index, m)| DisplayInfo {
                    id: index.to_string(),
                    name: m.name,
                    width: u32::from(m.rect.width),
                    height: u32::from(m.rect.height),
                    primary: m.primary,
                })
                .collect())
        })
        .await
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>> {
        blocking(|| {
            let (conn, number) = connect()?;
            let screen = screen_of(&conn, number)?;
            Ok(client_windows(&conn, &screen)?
                .into_iter()
                .filter_map(|(window, title, app)| {
                    let rect = window_rect(&conn, &screen, window).ok()?;
                    Some(WindowInfo {
                        id: window.to_string(),
                        title,
                        app,
                        width: u32::from(rect.width),
                        height: u32::from(rect.height),
                    })
                })
                .collect())
        })
        .await
    }

    async fn capture_display(&self, id: Option<&str>) -> Result<Captured> {
        let id = id.map(str::to_owned);
        blocking(move || {
            let (conn, number) = connect()?;
            let screen = screen_of(&conn, number)?;
            let monitors = monitors(&conn, &screen);
            let monitor = match &id {
                Some(id) => id
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| monitors.get(index))
                    .ok_or_else(|| {
                        VisionError::NotFound("the display is no longer connected".into())
                    })?,
                None => monitors
                    .iter()
                    .find(|m| m.primary)
                    .or_else(|| monitors.first())
                    .ok_or_else(|| VisionError::NotFound("no display found".into()))?,
            };
            Ok(Captured {
                image: grab(&conn, conn.setup(), &screen, monitor.rect)?,
                label: monitor.name.clone(),
            })
        })
        .await
    }

    async fn capture_window(&self, id: &str) -> Result<Captured> {
        let window: Window = id
            .parse()
            .map_err(|_| VisionError::NotFound("unknown window".into()))?;
        blocking(move || {
            let (conn, number) = connect()?;
            let screen = screen_of(&conn, number)?;
            let (_, title, _) = client_windows(&conn, &screen)?
                .into_iter()
                .find(|(w, _, _)| *w == window)
                .ok_or_else(|| VisionError::NotFound("the window was closed or hidden".into()))?;
            let rect = window_rect(&conn, &screen, window)?;
            Ok(Captured {
                image: grab(&conn, conn.setup(), &screen, rect)?,
                label: title,
            })
        })
        .await
    }

    async fn capture_selection(&self) -> Result<Captured> {
        Err(VisionError::Unsupported(
            "Choose a display or a window from the list".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_32_bit_little_endian_pixels() {
        // Two pixels of BGRX: pure red, pure blue; rows padded to 32 bits.
        let data = [0, 0, 255, 0, 255, 0, 0, 0];
        let layout = Layout {
            width: 2,
            height: 1,
            bits_per_pixel: 32,
            scanline_pad: 32,
            lsb_first: true,
            masks: (0x00FF_0000, 0x0000_FF00, 0x0000_00FF),
        };
        let image = to_rgba(&data, layout).expect("image");
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(1, 0).0, [0, 0, 255, 255]);
    }

    #[test]
    fn decodes_24_bit_rows_with_padding() {
        // 1×2 image, 3 bytes per pixel padded to 4 bytes per row, big endian.
        let data = [0x10, 0x20, 0x30, 0, 0xFF, 0x80, 0x00, 0];
        let layout = Layout {
            width: 1,
            height: 2,
            bits_per_pixel: 24,
            scanline_pad: 32,
            lsb_first: false,
            masks: (0x00FF_0000, 0x0000_FF00, 0x0000_00FF),
        };
        let image = to_rgba(&data, layout).expect("image");
        assert_eq!(image.get_pixel(0, 0).0, [0x10, 0x20, 0x30, 255]);
        assert_eq!(image.get_pixel(0, 1).0, [0xFF, 0x80, 0x00, 255]);
        assert!(to_rgba(&data[..4], layout).is_err(), "short data");
    }

    #[test]
    fn scales_narrow_channels_to_eight_bits() {
        // RGB565: full red, half green.
        assert_eq!(channel(0xF800, 0xF800), 255);
        assert_eq!(channel(0x0400, 0x07E0), 130);
        assert_eq!(channel(0, 0x001F), 0);
        assert_eq!(channel(0x12, 0), 0);
    }
}
