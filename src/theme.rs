//! Reading theme for the chat window.
//!
//! Body text stays a step softer than headings and bold, code sits on a darker
//! inset, and the sidebar/top bar share a darker chrome than the panes.

use eframe::egui::{self, Color32, FontFamily, FontId, Rounding, Stroke, TextStyle};

pub const ACCENT: Color32 = Color32::from_rgb(140, 186, 255);
pub const ACCENT_FILL: Color32 = Color32::from_rgb(72, 122, 210);
pub const CHROME: Color32 = Color32::from_rgb(18, 20, 25);
pub const CONTENT: Color32 = Color32::from_rgb(30, 33, 40);
pub const TEXT: Color32 = Color32::from_rgb(206, 212, 222);
pub const STRONG: Color32 = Color32::from_rgb(245, 247, 250);
pub const MUTED: Color32 = Color32::from_rgb(148, 156, 170);
pub const USER: Color32 = Color32::from_rgb(142, 186, 255);
pub const ASSISTANT: Color32 = Color32::from_rgb(142, 214, 176);
pub const DANGER: Color32 = Color32::from_rgb(240, 140, 140);

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.hyperlink_color = ACCENT;
    visuals.faint_bg_color = Color32::from_rgb(40, 46, 58);
    visuals.extreme_bg_color = Color32::from_rgb(14, 16, 20);
    visuals.code_bg_color = Color32::from_rgb(42, 50, 66);
    visuals.warn_fg_color = Color32::from_rgb(242, 176, 96);
    visuals.error_fg_color = DANGER;
    visuals.window_fill = CHROME;
    visuals.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(48, 56, 70));
    visuals.window_rounding = Rounding::same(8.0);
    visuals.menu_rounding = Rounding::same(8.0);
    visuals.panel_fill = CONTENT;
    visuals.text_cursor = Stroke::new(2.0_f32, ACCENT);
    visuals.selection.bg_fill = Color32::from_rgb(46, 74, 120);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.striped = true;

    let hairline = Color32::from_rgb(58, 66, 82);
    let button = Color32::from_rgb(46, 52, 66);
    let button_hover = Color32::from_rgb(58, 68, 88);
    let round = Rounding::same(6.0);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, hairline);
    widgets.noninteractive.bg_fill = CONTENT;
    widgets.noninteractive.weak_bg_fill = CONTENT;
    widgets.noninteractive.rounding = round;
    widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(220, 226, 234));
    widgets.inactive.bg_fill = button;
    widgets.inactive.weak_bg_fill = button;
    widgets.inactive.bg_stroke = Stroke::new(1.0_f32, hairline);
    widgets.inactive.rounding = round;
    widgets.inactive.expansion = 0.0;
    widgets.hovered.fg_stroke = Stroke::new(1.5_f32, STRONG);
    widgets.hovered.bg_fill = button_hover;
    widgets.hovered.weak_bg_fill = button_hover;
    widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(120, 142, 176));
    widgets.hovered.rounding = round;
    widgets.hovered.expansion = 0.0;
    widgets.active.fg_stroke = Stroke::new(1.0_f32, STRONG);
    widgets.active.bg_fill = ACCENT_FILL;
    widgets.active.weak_bg_fill = ACCENT_FILL;
    widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    widgets.active.rounding = round;
    widgets.active.expansion = 0.0;
    widgets.open.fg_stroke = Stroke::new(1.0_f32, STRONG);
    widgets.open.bg_fill = Color32::from_rgb(36, 42, 54);
    widgets.open.weak_bg_fill = Color32::from_rgb(36, 42, 54);
    widgets.open.bg_stroke = Stroke::new(1.0_f32, hairline);
    widgets.open.rounding = round;

    let mut style = (*ctx.style()).clone();
    style.visuals = visuals;
    style.text_styles = [
        (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(13.5, FontFamily::Proportional)),
        (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(13.5, FontFamily::Monospace)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(8.0, 5.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    style.interaction.resize_grab_radius_side = 5.0;
    ctx.set_style(style);
}

pub fn chrome_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::side_top_panel(&ctx.style())
        .fill(CHROME)
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
}
