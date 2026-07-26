//! Font installation, including a CJK fallback for the Chinese UI text.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use iconflow::fonts;

pub(crate) fn install_fonts(ctx: &egui::Context) {
    let mut definitions = FontDefinitions::default();
    let fallback_fonts = definitions.font_data.keys().cloned().collect::<Vec<_>>();
    for font in fonts() {
        definitions.font_data.insert(
            font.family.to_owned(),
            std::sync::Arc::new(FontData::from_static(font.bytes)),
        );
        let family = definitions
            .families
            .entry(FontFamily::Name(font.family.into()))
            .or_default();
        family.push(font.family.to_owned());
        family.extend(fallback_fonts.iter().cloned());
    }

    if let Some(bytes) = load_cjk_font() {
        definitions.font_data.insert(
            "crossdesk-cjk".into(),
            std::sync::Arc::new(FontData::from_owned(bytes)),
        );
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            definitions
                .families
                .entry(family)
                .or_default()
                .insert(0, "crossdesk-cjk".into());
        }
    } else {
        log::warn!("no supported CJK system font found");
    }
    ctx.set_fonts(definitions);
}

pub(crate) fn load_cjk_font() -> Option<Vec<u8>> {
    #[cfg(windows)]
    let candidates = {
        let windows_root = std::env::var("WINDIR")
            .or_else(|_| std::env::var("SystemRoot"))
            .unwrap_or_else(|_| r"C:\Windows".to_owned());
        [
            Some(format!("{windows_root}\\Fonts\\msyh.ttc")),
            Some(format!("{windows_root}\\Fonts\\msyhbd.ttc")),
        ]
    };
    #[cfg(target_os = "macos")]
    let candidates = [
        Some("/System/Library/Fonts/PingFang.ttc".to_owned()),
        Some("/System/Library/Fonts/STHeiti Medium.ttc".to_owned()),
    ];
    #[cfg(not(any(windows, target_os = "macos")))]
    let candidates = [
        Some("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".to_owned()),
        Some("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc".to_owned()),
    ];

    candidates
        .into_iter()
        .flatten()
        .find_map(|path| std::fs::read(path).ok())
}
