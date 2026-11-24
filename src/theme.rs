//! Light/dark palette that follows the Windows theme and accent color.

use windows::{
    Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    core::w,
};

use crate::gfx::Color;

#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    pub accent: Color,
    /// Text on an accent fill.
    pub on_accent: Color,
    /// Window background behind the cards (drawn over the Mica backdrop).
    pub backdrop: Color,
    pub card: Color,
    pub card_border: Color,
    pub card_hover_border: Color,
    pub text: Color,
    pub text2: Color,
    pub text3: Color,
    pub track: Color,
    pub chip: Color,
    pub chip_border: Color,
    pub hover: Color,
    pub press: Color,
    pub separator: Color,
    pub green: Color,
    pub orange: Color,
    pub red: Color,
    pub scrim: Color,
    pub sheet: Color,
    pub field: Color,
}

impl Theme {
    pub fn current() -> Theme {
        let dark = reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("AppsUseLightTheme"))
            .map(|v| v == 0)
            .unwrap_or(false);
        // AccentColor is stored as 0xAABBGGRR.
        let accent = reg_dword(w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor"))
            .map(|v| Color::rgba((v & 255) as u8, ((v >> 8) & 255) as u8, ((v >> 16) & 255) as u8, 1.0))
            .unwrap_or(Color::hex(0x0067C0));
        let accent = if dark { lighten(accent, 0.35) } else { darken(accent, 0.1) };
        let luma = 0.299 * accent.0 + 0.587 * accent.1 + 0.114 * accent.2;
        let on_accent = if luma > 0.6 { Color::hex(0x111111) } else { Color::white(1.0) };
        if dark {
            Theme {
                dark,
                accent,
                on_accent,
                backdrop: Color::rgba(32, 32, 32, 0.55),
                card: Color::rgba(255, 255, 255, 0.055),
                card_border: Color::rgba(255, 255, 255, 0.08),
                card_hover_border: Color::rgba(255, 255, 255, 0.18),
                text: Color::hex(0xF5F5F5),
                text2: Color::rgba(255, 255, 255, 0.66),
                text3: Color::rgba(255, 255, 255, 0.42),
                track: Color::rgba(255, 255, 255, 0.10),
                chip: Color::rgba(255, 255, 255, 0.06),
                chip_border: Color::rgba(255, 255, 255, 0.09),
                hover: Color::rgba(255, 255, 255, 0.07),
                press: Color::rgba(255, 255, 255, 0.04),
                separator: Color::rgba(255, 255, 255, 0.08),
                green: Color::hex(0x6CCB5F),
                orange: Color::hex(0xF7A35C),
                red: Color::hex(0xFF8A80),
                scrim: Color::rgba(0, 0, 0, 0.45),
                sheet: Color::hex(0x2B2B2B),
                field: Color::rgba(255, 255, 255, 0.06),
            }
        } else {
            Theme {
                dark,
                accent,
                on_accent,
                backdrop: Color::rgba(243, 243, 243, 0.55),
                card: Color::rgba(255, 255, 255, 0.92),
                card_border: Color::rgba(0, 0, 0, 0.07),
                card_hover_border: Color::rgba(0, 0, 0, 0.16),
                text: Color::hex(0x1A1A1A),
                text2: Color::rgba(0, 0, 0, 0.60),
                text3: Color::rgba(0, 0, 0, 0.40),
                track: Color::rgba(0, 0, 0, 0.08),
                chip: Color::rgba(0, 0, 0, 0.035),
                chip_border: Color::rgba(0, 0, 0, 0.07),
                hover: Color::rgba(0, 0, 0, 0.05),
                press: Color::rgba(0, 0, 0, 0.08),
                separator: Color::rgba(0, 0, 0, 0.07),
                green: Color::hex(0x2E8B3E),
                orange: Color::hex(0xC65D00),
                red: Color::hex(0xC42B1C),
                scrim: Color::rgba(0, 0, 0, 0.30),
                sheet: Color::hex(0xFBFBFB),
                field: Color::rgba(0, 0, 0, 0.035),
            }
        }
    }
}

fn reg_dword(key: windows::core::PCWSTR, value: windows::core::PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(data)
}

fn lighten(c: Color, t: f32) -> Color {
    Color(c.0 + (1.0 - c.0) * t, c.1 + (1.0 - c.1) * t, c.2 + (1.0 - c.2) * t, c.3)
}

fn darken(c: Color, t: f32) -> Color {
    Color(c.0 * (1.0 - t), c.1 * (1.0 - t), c.2 * (1.0 - t), c.3)
}
