use std::sync::Arc;

use slint::fontique_07::fontique;

const CJK_FALLBACK_SCRIPTS: &[&str] = &["Hani", "Bopo", "Kana", "Hang"];

pub(crate) struct EmbeddedFontAsset {
    pub(crate) file_name: &'static str,
    pub(crate) data: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/embedded_ui_fonts.rs"));

pub(crate) fn register_embedded_ui_fonts() {
    if EMBEDDED_UI_FONTS.is_empty() {
        return;
    }

    let mut collection = slint::fontique_07::shared_collection();

    for font in EMBEDDED_UI_FONTS {
        let blob = fontique::Blob::new(Arc::new(font.data.to_vec()));
        let registered = collection.register_fonts(blob, None);
        if registered.is_empty() {
            eprintln!(
                "CliGJ: embedded font '{}' did not expose any usable faces",
                font.file_name
            );
            continue;
        }
        for script in CJK_FALLBACK_SCRIPTS {
            collection.append_fallbacks(
                fontique::FallbackKey::new(*script, None),
                registered.iter().map(|(family_id, _)| *family_id),
            );
        }
        eprintln!("CliGJ: registered embedded font '{}'", font.file_name);
    }
}

