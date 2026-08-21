//! Design tokens and shared painting helpers for both light and dark themes.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Frame, Pos2, Rect, RichText, Sense, Shape,
    Stroke, StrokeKind, Theme, Vec2,
};

pub const TITLE_SIZE: f32 = 30.0;
pub const HEADING_SIZE: f32 = 16.0;
pub const BODY_SIZE: f32 = 13.5;
pub const CAPTION_SIZE: f32 = 11.0;

pub const CARD_RADIUS: u8 = 14;
pub const CANVAS_RADIUS: u8 = 18;
pub const WIDGET_RADIUS: u8 = 10;

pub const ACCENT: Color32 = Color32::from_rgb(34, 211, 238);
pub const ACCENT_HOT: Color32 = Color32::from_rgb(94, 234, 212);

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
    pub accent_hot: Color32,
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
            bg: Color32::from_rgb(10, 10, 10),
            bg_canvas: Color32::from_rgb(14, 14, 14),
            surface: Color32::from_rgb(20, 20, 20),
            surface_raised: Color32::from_rgb(26, 26, 26),
            border: Color32::from_rgb(40, 40, 40),
            border_strong: Color32::from_rgb(64, 64, 64),
            text: Color32::from_rgb(237, 237, 237),
            text_secondary: Color32::from_rgb(163, 163, 163),
            text_muted: Color32::from_rgb(115, 115, 115),
            accent: ACCENT,
            accent_hot: ACCENT_HOT,
            accent_soft: Color32::from_rgb(14, 38, 42),
            success: Color32::from_rgb(52, 211, 153),
            warning: Color32::from_rgb(251, 191, 36),
            danger: Color32::from_rgb(248, 113, 113),
            glow: ACCENT,
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color32::from_rgb(250, 250, 250),
            bg_canvas: Color32::from_rgb(245, 245, 245),
            surface: Color32::WHITE,
            surface_raised: Color32::WHITE,
            border: Color32::from_rgb(229, 229, 229),
            border_strong: Color32::from_rgb(212, 212, 212),
            text: Color32::from_rgb(10, 10, 10),
            text_secondary: Color32::from_rgb(82, 82, 82),
            text_muted: Color32::from_rgb(115, 115, 115),
            accent: Color32::from_rgb(8, 145, 178),
            accent_hot: Color32::from_rgb(13, 148, 136),
            accent_soft: Color32::from_rgb(224, 247, 250),
            success: Color32::from_rgb(5, 150, 105),
            warning: Color32::from_rgb(180, 83, 9),
            danger: Color32::from_rgb(190, 18, 60),
            glow: Color32::from_rgb(8, 145, 178),
        }
    }
}

pub fn palette(ui: &egui::Ui) -> Palette {
    Palette::of(ui)
}

pub fn title_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into())
        .size(TITLE_SIZE)
        .strong()
        .extra_letter_spacing(-0.5)
}

pub fn heading_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(HEADING_SIZE).strong()
}

pub fn caption_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into())
        .size(CAPTION_SIZE)
        .extra_letter_spacing(1.6)
        .strong()
}

pub fn section_label(ui: &mut egui::Ui, text: impl Into<String>) {
    let palette = palette(ui);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(14.0, 2.0), Sense::hover());
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(1),
            accent_gradient_stop(palette, 0.0),
        );
        ui.label(caption_text(text).color(palette.text_muted));
    });
}

/// Rounded card frame used by list rows, forms and status panels.
pub fn card_frame(ui: &egui::Ui) -> Frame {
    let palette = palette(ui);
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(18)
}

/// Slimmer variant of [`card_frame`] for dense list rows.
pub fn row_frame(ui: &egui::Ui) -> Frame {
    card_frame(ui).inner_margin(12)
}

/// Small pill-shaped status badge: drawn dot plus text.
pub fn status_badge(ui: &mut egui::Ui, color: Color32, text: &str) -> egui::Response {
    Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.28)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                status_dot(ui, color);
                ui.label(
                    RichText::new(text)
                        .size(CAPTION_SIZE)
                        .extra_letter_spacing(0.4)
                        .color(color),
                );
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
    ui.painter().circle_filled(rect.center(), 3.0, color);
    ui.painter()
        .circle_filled(rect.center(), 5.5, color.gamma_multiply(0.25));
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
    for layer in 1..=4 {
        let expand = layer as f32 * 2.5;
        let alpha = 0.22 / (layer as f32).powi(2);
        painter.rect_stroke(
            rect.expand(expand),
            CornerRadius::same(radius + layer as u8 * 2),
            Stroke::new(2.0, color.gamma_multiply(alpha)),
            StrokeKind::Outside,
        );
    }
}

/// One stop of the accent gradient: `t = 0.0` is the electric indigo end,
/// `t = 1.0` the violet end.
pub fn accent_gradient_stop(palette: Palette, t: f32) -> Color32 {
    lerp_color(palette.accent, palette.accent_hot, t)
}

/// Paint a horizontal accent gradient line inside `rect`.
pub fn accent_gradient_line(painter: &egui::Painter, rect: Rect, palette: Palette) {
    let steps = 24;
    let step_w = rect.width() / steps as f32;
    for i in 0..steps {
        let t = i as f32 / (steps - 1) as f32;
        let r = Rect::from_min_size(
            Pos2::new(rect.left() + i as f32 * step_w, rect.top()),
            Vec2::new(step_w + 0.5, rect.height()),
        );
        painter.rect_filled(r, CornerRadius::ZERO, accent_gradient_stop(palette, t));
    }
}

/// Layered ambient glow: two offset radial blooms, one indigo one violet,
/// giving the canvas an aurora-like depth instead of a flat fill.
pub fn ambient_glow(painter: &egui::Painter, rect: Rect, palette: Palette) {
    let left = Pos2::new(
        rect.left() + rect.width() * 0.28,
        rect.top() + rect.height() * 0.32,
    );
    let right = Pos2::new(
        rect.right() - rect.width() * 0.22,
        rect.bottom() - rect.height() * 0.25,
    );
    for (center, color) in [(left, palette.accent), (right, palette.accent_hot)] {
        for (radius, alpha) in [(190.0, 0.028), (130.0, 0.032), (80.0, 0.030)] {
            painter.circle_filled(center, radius, color.gamma_multiply(alpha));
        }
    }
}

/// Fine dot grid across a rect — the "engineering blueprint" texture that
/// keeps large empty areas from feeling flat.
pub fn dot_grid(painter: &egui::Painter, rect: Rect, color: Color32, spacing: f32) {
    let clip = painter.clip_rect().intersect(rect);
    let start_x = (clip.left() / spacing).ceil() * spacing;
    let start_y = (clip.top() / spacing).ceil() * spacing;
    let mut y = start_y;
    while y < clip.bottom() {
        let mut x = start_x;
        while x < clip.right() {
            painter.circle_filled(Pos2::new(x, y), 0.75, color);
            x += spacing;
        }
        y += spacing;
    }
}

pub fn configure_style(ctx: &egui::Context) {
    for (theme, palette) in [
        (Theme::Dark, Palette::dark()),
        (Theme::Light, Palette::light()),
    ] {
        let mut style = (*ctx.style_of(theme)).clone();
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(14.0, 8.0);
        style.animation_time = 0.14;
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
        style.visuals.widgets.inactive.weak_bg_fill = palette.surface;
        style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border);
        style.visuals.widgets.hovered.weak_bg_fill = palette.surface_raised;
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.border_strong);
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.floating = true;
        ctx.set_style_of(theme, style);
    }
}

/// Font id for a Lucide icon glyph at a given size.
pub fn icon_font(family: &str, size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(family.into()))
}
