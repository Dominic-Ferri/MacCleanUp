//! Visual identity: the slate "tunnel" of the mole icon, its cyan headlamp for
//! selection, and ore amber for anything that moves files to Trash.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};
use std::sync::Arc;

pub const BEDROCK: Color32 = Color32::from_rgb(0x1C, 0x23, 0x29);
pub const WELL: Color32 = Color32::from_rgb(0x14, 0x1A, 0x1F);
pub const SEAM: Color32 = Color32::from_rgb(0x24, 0x2D, 0x35);
pub const SHALE: Color32 = Color32::from_rgb(0x34, 0x40, 0x4A);
pub const RIDGE: Color32 = Color32::from_rgb(0x45, 0x53, 0x5F);
pub const CHALK: Color32 = Color32::from_rgb(0xE8, 0xED, 0xF0);
pub const SILT: Color32 = Color32::from_rgb(0x93, 0xA2, 0xAE);
pub const HEADLAMP: Color32 = Color32::from_rgb(0x66, 0xD6, 0xE8);
pub const ORE: Color32 = Color32::from_rgb(0xE3, 0xA9, 0x4B);
pub const FLARE: Color32 = Color32::from_rgb(0xE8, 0x65, 0x6F);

/// Mineral tones for storage strata and treemap tiles. Muted so the headlamp
/// outline of the selected tile stays the brightest thing on screen.
pub const STRATA: [Color32; 10] = [
    Color32::from_rgb(0x4F, 0x7A, 0x8C),
    Color32::from_rgb(0x7E, 0x6A, 0x96),
    Color32::from_rgb(0x67, 0x86, 0x58),
    Color32::from_rgb(0x9C, 0x7B, 0x4C),
    Color32::from_rgb(0x58, 0x68, 0x96),
    Color32::from_rgb(0x88, 0x5E, 0x5E),
    Color32::from_rgb(0x4E, 0x86, 0x7C),
    Color32::from_rgb(0x96, 0x6A, 0x84),
    Color32::from_rgb(0x6E, 0x78, 0x86),
    Color32::from_rgb(0x8E, 0x86, 0x5A),
];

pub fn condensed(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("condensed".into()))
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}

const AVENIR: &str = "/System/Library/Fonts/Avenir Next.ttc";
const AVENIR_CONDENSED: &str = "/System/Library/Fonts/Avenir Next Condensed.ttc";

/// Loads Avenir Next from macOS at run time (it ships with every supported
/// macOS release and is not bundled). Falls back to egui's own fonts when a
/// file is missing, so the named families always resolve.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut load = |key: &str, path: &str, index: u32| -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        fonts.font_data.insert(
            key.to_string(),
            Arc::new(FontData::from_blob(Arc::new(bytes), index)),
        );
        Some(key.to_string())
    };
    // Face indices from `fc-scan`: 7 Regular, 5 Medium, 2 Demi Bold.
    let regular = load("avenir-regular", AVENIR, 7);
    let medium_face = load("avenir-medium", AVENIR, 5);
    let condensed_face = load("avenir-condensed-demi", AVENIR_CONDENSED, 2);
    let family = |primary: Option<String>| {
        let mut list: Vec<String> = primary.into_iter().collect();
        list.extend(fallback.iter().cloned());
        list
    };
    fonts
        .families
        .insert(FontFamily::Proportional, family(regular));
    fonts
        .families
        .insert(FontFamily::Name("medium".into()), family(medium_face));
    fonts
        .families
        .insert(FontFamily::Name("condensed".into()), family(condensed_face));
    ctx.set_fonts(fonts);

    // The same dark style is used whatever the system appearance.
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, medium(14.0)),
            (TextStyle::Heading, condensed(26.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 5.0);
        style.spacing.interact_size.y = 26.0;

        let visuals = &mut style.visuals;
        *visuals = egui::Visuals::dark();
        visuals.panel_fill = BEDROCK;
        visuals.window_fill = SEAM;
        // Text fields sit a step darker than the page so they read as inputs.
        visuals.extreme_bg_color = WELL;
        visuals.text_edit_bg_color = Some(WELL);
        visuals.faint_bg_color = SEAM;
        visuals.override_text_color = Some(CHALK);
        visuals.hyperlink_color = HEADLAMP;
        visuals.selection.bg_fill = HEADLAMP.gamma_multiply(0.22);
        visuals.selection.stroke = Stroke::new(1.0, HEADLAMP);
        visuals.window_stroke = Stroke::new(1.0, SHALE);
        visuals.window_corner_radius = CornerRadius::same(10);
        visuals.menu_corner_radius = CornerRadius::same(8);
        let widgets = &mut visuals.widgets;
        for state in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.open,
            &mut widgets.noninteractive,
        ] {
            state.corner_radius = CornerRadius::same(5);
        }
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, SHALE);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, CHALK);
        widgets.inactive.weak_bg_fill = SHALE;
        widgets.inactive.bg_fill = SHALE;
        widgets.inactive.fg_stroke = Stroke::new(1.0, CHALK);
        widgets.hovered.weak_bg_fill = RIDGE;
        widgets.hovered.bg_fill = RIDGE;
        widgets.hovered.bg_stroke = Stroke::new(1.0, SILT);
        widgets.active.weak_bg_fill = RIDGE;
        widgets.active.bg_stroke = Stroke::new(1.0, HEADLAMP);
    });
}
