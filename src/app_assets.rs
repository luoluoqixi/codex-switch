use gpui_kit::{AssetSource, Result, SharedString, assets};
use std::borrow::Cow;

assets::icon_assets!(
    ExtraIcons,
    [
        FilePen,
        FileUp,
        Power,
        Save,
        ArrowLeftRight,
        FolderClock,
        SlidersHorizontal,
        Trash,
        X
    ]
);

pub(crate) struct AppAssets;

/// Also give unbundled `cargo run` processes the application's Dock icon.
#[cfg(target_os = "macos")]
pub(crate) fn set_macos_application_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(include_bytes!("../assets/icon-macos.png"));
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: A non-nil image is supplied on the main thread. AppKit retains it.
        unsafe {
            NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image));
        }
    }
}

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
