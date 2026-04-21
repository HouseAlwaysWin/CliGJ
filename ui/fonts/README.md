Drop packaged terminal fallback fonts in this folder.

Supported file types:
- `.ttf`
- `.otf`
- `.ttc`

Build behavior:
- `build.rs` scans `ui/fonts`
- every matching font file is embedded into the build output
- at app startup, CliGJ registers those fonts with Slint's shared font collection
- the registered families are appended as fallback fonts for `Hani`, `Bopo`, `Kana`, and `Hang`

Recommended use:
- put one dedicated CJK monospace font here, for example a Sarasa Mono / Sarasa Fixed variant
- keep the normal terminal font set to `DejaVu Sans Mono` or `Cascadia Mono`
- use the in-app `中文補字字型` setting only for system-font fallback tuning

Notes:
- this folder is currently empty by default, so builds behave exactly as before until you add a font file
- if you add or replace a font here, rebuild the app so the embedded font list is regenerated
