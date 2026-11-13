//! Initial window geometry, shared by every independent utility window.
use gpui_kit::*;

pub(crate) fn centered_on_main(
    requested_size: Size<Pixels>,
    cx: &mut App,
) -> (Bounds<Pixels>, Option<DisplayId>) {
    let main = crate::shell::main_window_handle(cx).and_then(|handle| {
        cx.update_window(handle, |_, window, cx| {
            window
                .is_visible()
                .then(|| (window.bounds(), window.display(cx)))
        })
        .ok()
        .flatten()
    });
    let display = main
        .as_ref()
        .and_then(|(_, display)| display.clone())
        .or_else(|| current_display_id(cx).and_then(|id| cx.find_display(id)))
        .or_else(|| cx.primary_display());
    let Some(display) = display else {
        return (Bounds::centered(None, requested_size, cx), None);
    };
    let visible = display.visible_bounds();
    let size = size(
        requested_size.width.min(visible.size.width),
        requested_size.height.min(visible.size.height),
    );
    let center = main
        .map(|(bounds, _)| bounds.center())
        .unwrap_or_else(|| visible.center());
    let mut bounds = Bounds::centered_at(center, size);
    bounds.origin.x = bounds
        .origin
        .x
        .max(visible.origin.x)
        .min(visible.origin.x + visible.size.width - size.width);
    bounds.origin.y = bounds
        .origin
        .y
        .max(visible.origin.y)
        .min(visible.origin.y + visible.size.height - size.height);
    (bounds, Some(display.id()))
}

pub(crate) fn current_display_id(cx: &App) -> Option<DisplayId> {
    cursor_display_id()
        .filter(|id| cx.find_display(*id).is_some())
        .or_else(|| cx.primary_display().map(|display| display.id()))
}

#[cfg(target_os = "windows")]
fn cursor_display_id() -> Option<DisplayId> {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint},
        UI::WindowsAndMessaging::GetCursorPos,
    };
    let mut cursor = POINT::default();
    // HMONITOR is also the Windows GPUI PlatformDisplay identifier.
    unsafe {
        GetCursorPos(&mut cursor).ok()?;
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        (!monitor.is_invalid()).then(|| DisplayId::new(monitor.0 as u64))
    }
}

#[cfg(target_os = "macos")]
fn cursor_display_id() -> Option<DisplayId> {
    use std::ffi::c_void;
    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const c_void) -> *const c_void;
        fn CGEventGetLocation(event: *const c_void) -> CGPoint;
        fn CGGetDisplaysWithPoint(
            point: CGPoint,
            maximum: u32,
            displays: *mut u32,
            count: *mut u32,
        ) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *const c_void);
    }
    // CoreGraphics and GPUI use CGDirectDisplayID and global screen coordinates.
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return None;
        }
        let cursor = CGEventGetLocation(event);
        CFRelease(event);
        let mut id = 0;
        let mut count = 0;
        let status = CGGetDisplaysWithPoint(cursor, 1, &mut id, &mut count);
        (status == 0 && count > 0).then(|| DisplayId::new(id as u64))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn cursor_display_id() -> Option<DisplayId> {
    None
}
