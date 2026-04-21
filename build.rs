use std::fs::File;
use std::path::{Path, PathBuf};

fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .with_bundled_translations("translations")
            // Hand-written `.po` has plain msgids only; default `ComponentName` context would leave `en`/`zh_TW` empty.
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("failed to compile Slint UI");
    embed_windows_exe_icon();
}

/// Multi-resolution .ico from `src/asset/logo3.png` + `winres` (Windows exe icon).
fn embed_windows_exe_icon() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let png_path = manifest_dir.join("src/asset/logo3.png");
    if !png_path.is_file() {
        eprintln!("CliGJ build: skip exe icon — missing {}", png_path.display());
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico_path = out_dir.join("cligj_logo3.ico");
    if let Err(e) = png_to_ico(&png_path, &ico_path) {
        eprintln!("CliGJ build: failed to write {}: {e}", ico_path.display());
        return;
    }

    let mut res = winres::WindowsResource::new();
    res.set_icon(ico_path.to_string_lossy().as_ref());
    if let Err(e) = res.compile() {
        eprintln!("CliGJ build: winres (exe icon) failed: {e}");
    }
}

fn png_to_ico(png_path: &Path, ico_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let img = image::open(png_path)?.into_rgba8();
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);

    for size in [16_u32, 24, 32, 48, 64, 128, 256] {
        let rgba = if (img.width(), img.height()) == (size, size) {
            img.clone()
        } else {
            image::imageops::resize(
                &img,
                size,
                size,
                image::imageops::FilterType::Lanczos3,
            )
        };
        let raw = rgba.into_raw();
        let icon_image =
            ico::IconImage::from_rgba_data(size as u32, size as u32, raw);
        icon_dir.add_entry(ico::IconDirEntry::encode(&icon_image)?);
    }

    let mut out = File::create(ico_path)?;
    icon_dir.write(&mut out)?;
    Ok(())
}
