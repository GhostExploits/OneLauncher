use freya::prelude::Color;

pub const MODRINTH_COLOR: Color = Color::from_rgb(27, 217, 106);
pub const CURSEFORGE_COLOR: Color = Color::from_rgb(241, 100, 54);

pub fn page() -> Color {
    Color::from_rgb(11, 13, 16)
}

pub fn page_elevated() -> Color {
    Color::from_rgb(18, 22, 27)
}

pub fn fg_primary() -> Color {
    Color::from_rgb(242, 245, 248)
}

pub fn fg_primary_hover() -> Color {
    Color::from_rgb(255, 255, 255)
}

pub fn fg_primary_pressed() -> Color {
    Color::from_rgb(226, 231, 236)
}

pub fn fg_primary_disabled() -> Color {
    Color::from_rgb(148, 156, 166)
}

pub fn fg_secondary() -> Color {
    Color::from_rgb(136, 146, 158)
}

pub fn fg_secondary_hover() -> Color {
    Color::from_rgb(160, 170, 181)
}

pub fn fg_secondary_pressed() -> Color {
    Color::from_rgb(111, 121, 132)
}

/// A restrained steel accent: enough hue to separate actions from body text,
/// but far away from the neon-purple dashboard look this product is avoiding.
pub fn brand() -> Color {
    Color::from_rgb(86, 115, 146)
}

pub fn brand_hover() -> Color {
    Color::from_rgb(99, 130, 164)
}

pub fn brand_pressed() -> Color {
    Color::from_rgb(73, 99, 127)
}

pub fn brand_disabled() -> Color {
    Color::from_rgb(42, 54, 67)
}

pub fn ghost_overlay() -> Color {
    Color::from_argb(12, 255, 255, 255)
}

pub fn ghost_overlay_hover() -> Color {
    Color::from_argb(26, 255, 255, 255)
}

pub fn ghost_overlay_pressed() -> Color {
    Color::from_argb(38, 255, 255, 255)
}

pub fn component_bg() -> Color {
    Color::from_rgb(23, 28, 34)
}

pub fn component_bg_hover() -> Color {
    Color::from_rgb(29, 35, 42)
}

pub fn component_bg_pressed() -> Color {
    Color::from_rgb(35, 42, 50)
}

pub fn component_bg_disabled() -> Color {
    Color::from_rgb(15, 19, 23)
}

pub fn component_border() -> Color {
    Color::from_argb(18, 255, 255, 255)
}

pub fn component_border_hover() -> Color {
    Color::from_argb(25, 255, 255, 255)
}

pub fn component_border_pressed() -> Color {
    Color::from_argb(38, 255, 255, 255)
}

pub fn danger() -> Color {
    Color::from_rgb(204, 78, 82)
}

pub fn danger_hover() -> Color {
    Color::from_rgb(220, 92, 96)
}

pub fn danger_pressed() -> Color {
    Color::from_rgb(181, 65, 69)
}

pub fn danger_disabled() -> Color {
    Color::from_rgb(92, 44, 47)
}

pub fn success() -> Color {
    Color::from_rgb(74, 158, 112)
}

pub fn code_info() -> Color {
    Color::from_rgb(97, 175, 239)
}

pub fn code_warn() -> Color {
    Color::from_rgb(229, 192, 123)
}

pub fn code_error() -> Color {
    Color::from_rgb(224, 108, 117)
}

pub fn code_debug() -> Color {
    Color::from_rgb(152, 195, 121)
}

pub fn code_chat() -> Color {
    Color::from_rgb(198, 120, 221)
}

pub fn code_muted() -> Color {
    Color::from_rgb(120, 128, 140)
}

pub fn selection_bg() -> Color {
    Color::from_rgb(97, 175, 239).with_a(60)
}

pub fn toast_action() -> Color {
    Color::from_rgb(155, 161, 166)
}
