//! Design tokens and shared painting helpers for both light and dark themes.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Frame, Pos2, Rect, RichText, Sense, Shape,
    Stroke, StrokeKind, Theme, Vec2,
};

pub const TITLE_SIZE: f32 = 30.0;
pub const HEADING_SIZE: f32 = 17.0;
pub const BODY_SIZE: f32 = 14.0;
pub const CAPTION_SIZE: f32 = 12.0;

pub const CARD_RADIUS: u8 = 10;
pub const CANVAS_RADIUS: u8 = 12;
pub const WIDGET_RADIUS: u8 = 8;

pub const ACCENT: Color32 = Color32::from_rgb(37, 99, 235);

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub bg_canvas: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub glow: Color32,
}

impl Palette {
    pub fn of(ui: &egui::Ui) -> Self {
        if ui.visuals().dark_mode {
            Self::dark()
        } else {
            Self::light()
        }
    }

    pub fn dark() -> Self {
        Self {
            bg: Color32::from_rgb(11, 14, 20),
            bg_canvas: Color32::from_rgb(14, 18, 26),
            surface: Color32::from_rgb(21, 26, 35),
            surface_raised: Color32::from_rgb(27, 33, 44),
            border: Color32::from_rgb(38, 45, 58),
            border_strong: Color32::from_rgb(55, 65, 81),
            text: Color32::from_rgb(229, 231, 235),
            text_secondary: Color32::from_rgb(156, 163, 175),
            text_muted: Color32::from_rgb(107, 114, 128),
            accent: ACCENT,
            accent_soft: Color32::from_rgb(27, 46, 76),
            success: Color32::from_rgb(22, 163, 74),
            warning: Color32::from_rgb(217, 119, 6),
            danger: Color32::from_rgb(220, 38, 38),
            glow: ACCENT,
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color32::from_rgb(246, 247, 249),
            bg_canvas: Color32::from_rgb(238, 241, 245),
            surface: Color32::WHITE,
            surface_raised: Color32::WHITE,
            border: Color32::from_rgb(209, 213, 219),
            border_strong: Color32::from_rgb(156, 163, 175),
            text: Color32::from_rgb(17, 24, 39),
            text_secondary: Color32::from_rgb(75, 85, 99),
            text_muted: Color32::from_rgb(100, 116, 139),
            accent: ACCENT,
            accent_soft: Color32::from_rgb(239, 246, 255),
            success: Color32::from_rgb(22, 163, 74),
            warning: Color32::from_rgb(217, 119, 6),
            danger: Color32::from_rgb(220, 38, 38),
            glow: ACCENT,
        }
    }
}

pub fn palette(ui: &egui::Ui) -> Palette {
    Palette::of(ui)
}

pub fn title_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(TITLE_SIZE).strong()
}

pub fn heading_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(HEADING_SIZE).strong()
}

pub fn caption_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into())
        .size(CAPTION_SIZE)
        .extra_letter_spacing(0.8)
        .strong()
}

pub fn section_label(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(caption_text(text).color(palette(ui).text_muted));
}

/// Rounded card frame used by list rows, forms and status panels.
pub fn card_frame(ui: &egui::Ui) -> Frame {
    let palette = palette(ui);
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(16)
}

/// Slimmer variant of [`card_frame`] for dense list rows.
pub fn row_frame(ui: &egui::Ui) -> Frame {
    card_frame(ui).inner_margin(12)
}

/// Small pill-shaped status badge: drawn dot plus text.
pub fn status_badge(ui: &mut egui::Ui, color: Color32, text: &str) -> egui::Response {
    Frame::new()
        .fill(color.gamma_multiply(0.14))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                status_dot(ui, color);
                ui.label(RichText::new(text).size(CAPTION_SIZE).color(color));
            });
        })
        .response
}

/// Linear interpolation between two colors.
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// Drawn status dot replacing textual `●` markers.
pub fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// Dashed 1px outline used for empty screen slots on the layout canvas.
pub fn dashed_rect_stroke(painter: &egui::Painter, rect: Rect, stroke: Stroke) {
    let inset = stroke.width / 2.0;
    let rect = rect.shrink(inset + 1.0);
    let points = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    for shape in Shape::dashed_line(&points, stroke, 6.0, 5.0) {
        painter.add(shape);
    }
}

/// Soft accent glow around a rect: layered strokes fading outwards.
pub fn glow_stroke(painter: &egui::Painter, rect: Rect, radius: u8, color: Color32) {
    for layer in 1..=3 {
        let expand = layer as f32 * 2.0;
        let alpha = 0.28 / layer as f32;
        painter.rect_stroke(
            rect.expand(expand),
            CornerRadius::same(radius + layer as u8 * 2),
            Stroke::new(2.0, color.gamma_multiply(alpha)),
            StrokeKind::Outside,
        );
    }
}

/// Subtle radial accent glow centered in a rect (canvas backdrop).
pub fn radial_glow(painter: &egui::Painter, center: Pos2, color: Color32) {
    for (radius, alpha) in [(150.0, 0.05), (100.0, 0.05), (60.0, 0.04)] {
        painter.circle_filled(center, radius, color.gamma_multiply(alpha));
    }
}

pub fn configure_style(ctx: &egui::Context) {
    for (theme, palette) in [
        (Theme::Dark, Palette::dark()),
        (Theme::Light, Palette::light()),
    ] {
        let mut style = (*ctx.style_of(theme)).clone();
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 7.0);
        style.animation_time = 0.12;
        style.visuals.panel_fill = palette.bg;
        style.visuals.window_fill = palette.surface_raised;
        style.visuals.window_stroke = Stroke::new(1.0, palette.border);
        style.visuals.extreme_bg_color = palette.bg_canvas;
        style.visuals.selection.bg_fill = ACCENT;
        style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        let radius = CornerRadius::same(WIDGET_RADIUS);
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;
        style.visuals.widgets.open.corner_radius = radius;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = true;
        ctx.set_style_of(theme, style);
    }
}

/// Font id for a Lucide icon glyph at a given size.
pub fn icon_font(family: &str, size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(family.into()))
}
