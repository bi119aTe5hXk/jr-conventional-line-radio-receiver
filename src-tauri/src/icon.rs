use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSImage};
use objc2_foundation::NSData;

pub fn set_dock_icon() -> Result<(), Box<dyn std::error::Error>> {
    // Releases are standalone executables, without an .app bundle supplying
    // CFBundleIconFile. Tauri only sets the Dock icon itself in development.
    let main_thread = MainThreadMarker::new().ok_or("Dock icon must be set on the main thread")?;
    let data = NSData::with_bytes(include_bytes!("../icons/icon.icns"));
    let icon = NSImage::initWithData(NSImage::alloc(), &data)
        .ok_or("Unable to decode the application icon")?;
    let app = NSApplication::sharedApplication(main_thread);
    // SAFETY: the image is valid and this setup hook runs on the main thread.
    unsafe { app.setApplicationIconImage(Some(&icon)) };
    Ok(())
}
