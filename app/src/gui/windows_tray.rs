#[cfg(target_os = "windows")]
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[cfg(target_os = "windows")]
fn load_tray_icon() -> Option<Icon> {
    const TRAY_ICON_PNG: &[u8] = include_bytes!("../asset/logo3.png");
    let image = image::load_from_memory_with_format(TRAY_ICON_PNG, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).ok()
}

#[cfg(target_os = "windows")]
pub(crate) fn ensure_tray_icon(tray: &mut Option<TrayIcon>) -> Result<(), String> {
    if tray.is_some() {
        return Ok(());
    }

    let icon = load_tray_icon().ok_or_else(|| "failed to load tray icon image".to_string())?;
    let built = TrayIconBuilder::new()
        .with_tooltip("CliGJ")
        .with_icon(icon)
        .build()
        .map_err(|e| format!("tray create: {e}"))?;
    *tray = Some(built);
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn should_restore_from_event(event: TrayIconEvent) -> bool {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
        | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => true,
        _ => false,
    }
}

