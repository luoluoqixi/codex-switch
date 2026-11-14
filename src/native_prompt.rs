//! Platform-native confirmation with an explicit owner and cancellation default.
use anyhow::{Context as _, Result};
use gpui_kit::*;

pub(crate) fn confirm(
    title: String,
    detail: String,
    label: String,
    origin: Option<AnyWindowHandle>,
    cx: &mut App,
) -> Task<Result<bool>> {
    // Attach confirmation to the window that initiated the action, including
    // utility windows such as history. Tray actions can have no visible owner.
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let hwnd = origin.map_or(Ok(0), |origin| {
            cx.update_window(origin, |_, window, _| -> Result<usize> {
                let handle = HasWindowHandle::window_handle(window)
                    .map_err(|error| anyhow::anyhow!(crate::tr!("native_prompt.could_not_get_the_native_window_handle"; error = error)))?;
                match handle.as_raw() {
                    RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as usize),
                    _ => anyhow::bail!(crate::tr!("native_prompt.could_not_find_the_owner_window_for_the_windows")),
                }
            })
            .and_then(|result| result)
        });
        // Native modal loops may synchronously message their owner. Keep GPUI's
        // foreground thread pumping and never hold an entity lease while waiting.
        cx.background_spawn(async move { show_windows(hwnd?, &title, &detail, &label) })
    }
    #[cfg(not(target_os = "windows"))]
    {
        let Some(origin) = origin else {
            #[cfg(target_os = "macos")]
            return cx.background_spawn(async move { macos::confirm(&title, &detail, &label) });
            #[cfg(not(target_os = "macos"))]
            return cx.spawn(async |_| {
                anyhow::bail!(crate::tr!(
                    "native_prompt.standalone_system_confirmation_dialogs_are_not_supported_on_this"
                ))
            });
        };
        // macOS shows a native sheet attached to the initiating window.
        let receiver = cx.update_window(origin, |_, window, cx| {
            window.activate_window();
            window.prompt(
                PromptLevel::Warning,
                &title,
                Some(&detail),
                &[
                    PromptButton::cancel(crate::tr!("native_prompt.cancel")),
                    PromptButton::new(label),
                ],
                cx,
            )
        });
        cx.spawn(async move |_| {
            Ok(receiver?.await.context(crate::tr!(
                "native_prompt.the_system_confirmation_dialog_returned_no_result"
            ))? == 1)
        })
    }
}

#[cfg(target_os = "windows")]
fn show_windows(parent: usize, title: &str, detail: &str, label: &str) -> Result<bool> {
    use windows::{
        Win32::{
            Foundation::HWND,
            UI::{
                Controls::{
                    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TD_WARNING_ICON,
                    TDF_ALLOW_DIALOG_CANCELLATION, TDF_POSITION_RELATIVE_TO_WINDOW,
                    TaskDialogIndirect,
                },
                WindowsAndMessaging::IDCANCEL,
            },
        },
        core::{HSTRING, PCWSTR},
    };
    let title = HSTRING::from(title);
    let detail = HSTRING::from(detail);
    let label = HSTRING::from(label);
    let cancel = HSTRING::from(crate::tr!("native_prompt.cancel"));
    let caption = HSTRING::from(crate::APP_NAME);
    const CONFIRM_ID: i32 = 1000;
    // Explicit IDs keep Escape/X mapped to cancellation after swapping buttons.
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: CONFIRM_ID,
            pszButtonText: PCWSTR(label.as_ptr()),
        },
        TASKDIALOG_BUTTON {
            nButtonID: IDCANCEL.0,
            pszButtonText: PCWSTR(cancel.as_ptr()),
        },
    ];
    let mut config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: HWND(parent as *mut _),
        dwFlags: if parent == 0 {
            TDF_ALLOW_DIALOG_CANCELLATION
        } else {
            TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW
        },
        pszWindowTitle: PCWSTR(caption.as_ptr()),
        pszMainInstruction: PCWSTR(title.as_ptr()),
        pszContent: PCWSTR(detail.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: IDCANCEL.0,
        ..Default::default()
    };
    config.Anonymous1.pszMainIcon = TD_WARNING_ICON;
    let mut clicked = IDCANCEL.0;
    // Common Controls 6 comes from GPUI's embedded manifest.
    unsafe { TaskDialogIndirect(&config, Some(&mut clicked), None, None) }.context(crate::tr!(
        "native_prompt.could_not_open_the_windows_confirmation_dialog"
    ))?;
    Ok(clicked == CONFIRM_ID)
}

#[cfg(target_os = "macos")]
mod macos {
    use anyhow::{Context as _, Result, ensure};
    use std::ffi::c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            allocator: *const c_void,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> *const c_void;
        fn CFRelease(value: *const c_void);
        fn CFUserNotificationDisplayAlert(
            timeout: f64,
            flags: usize,
            icon: *const c_void,
            sound: *const c_void,
            localization: *const c_void,
            title: *const c_void,
            detail: *const c_void,
            default_button: *const c_void,
            alternate_button: *const c_void,
            other_button: *const c_void,
            response: *mut usize,
        ) -> i32;
    }

    struct NativeString(*const c_void);

    impl NativeString {
        fn new(value: &str) -> Result<Self> {
            const UTF8: u32 = 0x0800_0100;
            let length = isize::try_from(value.len()).context(crate::tr!(
                "native_prompt.the_confirmation_dialog_text_is_too_long"
            ))?;
            let string = unsafe {
                CFStringCreateWithBytes(std::ptr::null(), value.as_ptr(), length, UTF8, 0)
            };
            ensure!(
                !string.is_null(),
                crate::tr!("native_prompt.could_not_create_the_confirmation_dialog_text")
            );
            Ok(Self(string))
        }
    }

    impl Drop for NativeString {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) };
        }
    }

    pub(super) fn confirm(title: &str, detail: &str, label: &str) -> Result<bool> {
        let title = NativeString::new(title)?;
        let detail = NativeString::new(detail)?;
        let cancel = NativeString::new(crate::tr!("native_prompt.cancel"))?;
        let action = NativeString::new(label)?;
        const CAUTION_ALERT: usize = 2;
        const ALTERNATE_RESPONSE: usize = 1;
        let mut response = 0;
        // A standalone native alert needs no NSWindow. Run this blocking call on
        // a worker so the GPUI event loop and tray remain responsive.
        let status = unsafe {
            CFUserNotificationDisplayAlert(
                0.0,
                CAUTION_ALERT,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                title.0,
                detail.0,
                cancel.0,
                action.0,
                std::ptr::null(),
                &mut response,
            )
        };
        ensure!(
            status == 0,
            crate::tr!("native_prompt.could_not_open_the_macos_confirmation_dialog"; status = status)
        );
        Ok(response & 0x3 == ALTERNATE_RESPONSE)
    }
}
