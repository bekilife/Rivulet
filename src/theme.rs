//! Light/dark palette that follows the Windows app theme.

use windows::{
    Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    core::w,
};

use crate::gfx::Color;

#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    /// Downloads, primary buttons, focus.
    pub accent: Color,
    pub accent_soft: Color,
    /// Text on an accent fill.
    pub on_accent: Color,
    /// Uploads, finished and seeding downloads.
    pub green: Color,
    pub green_soft: Color,
    pub orange: Color,
    pub red: Color,
    /// Window background.
    pub backdrop: Color,
    /// Cards and button groups.
    pub card: Color,
    pub surface2: Color,
    pub card_border: Color,
    pub card_hover_border: Color,
    pub text: Color,
    pub text2: Color,
    pub text3: Color,
    pub track: Color,
    pub grid: Color,
    pub chip: Color,
    pub chip_border: Color,
    pub hover: Color,
    pub press: Color,
    pub separator: Color,
    pub scrim: Color,
    pub sheet: Color,
    pub field: Color,
    pub seg_on: Color,
    pub seg_on_text: Color,
    pub tip_bg: Color,
    pub tip_text: Color,
    pub link: Color,
    pub close_hover: Color,
}

impl Theme {
    pub fn current() -> Theme {
        let dark =
            reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("AppsUseLightTheme"))
                .map(|v| v == 0)
                .unwrap_or(false);
        let white = |a: f32| Color::white(a);
        let ink = |a: f32| Color::rgba(17, 24, 39, a);
        if dark {
            Theme {
                dark,
                accent: Color::hex(0x4FA3FF),
                accent_soft: Color::rgba(79, 163, 255, 0.15),
                on_accent: Color::hex(0x0B1A2E),
                green: Color::hex(0x45D483),
                green_soft: Color::rgba(69, 212, 131, 0.14),
                orange: Color::hex(0xF7A35C),
                red: Color::hex(0xFF7A7A),
                backdrop: Color::hex(0x141517),
                card: Color::hex(0x1D1E21),
                surface2: Color::hex(0x2A2B2F),
                card_border: white(0.07),
                card_hover_border: white(0.16),
                text: Color::hex(0xF2F3F5),
                text2: Color::hex(0xB1B4BB),
                text3: Color::hex(0x8B8F98),
                track: white(0.08),
                grid: white(0.07),
                chip: white(0.05),
                chip_border: white(0.08),
                hover: white(0.07),
                press: white(0.04),
                separator: white(0.07),
                scrim: Color(0.0, 0.0, 0.0, 0.5),
                sheet: Color::hex(0x1D1E21),
                field: white(0.05),
                seg_on: Color::hex(0xF2F3F5),
                seg_on_text: Color::hex(0x141517),
                tip_bg: Color::hex(0xF2F3F5),
                tip_text: Color::hex(0x141517),
                link: Color::hex(0x7CB8FF),
                close_hover: Color::hex(0xE5484D),
            }
        } else {
            Theme {
                dark,
                accent: Color::hex(0x1F6FEB),
                accent_soft: Color::rgba(31, 111, 235, 0.10),
                on_accent: Color::white(1.0),
                green: Color::hex(0x1E9E5A),
                green_soft: Color::rgba(30, 158, 90, 0.12),
                orange: Color::hex(0xC65D00),
                red: Color::hex(0xC42B1C),
                backdrop: Color::hex(0xEEF0F3),
                card: Color::hex(0xFFFFFF),
                surface2: Color::hex(0xF0F1F4),
                card_border: ink(0.08),
                card_hover_border: ink(0.18),
                text: Color::hex(0x14161A),
                text2: Color::hex(0x555B66),
                text3: Color::hex(0x6B717C),
                track: ink(0.08),
                grid: ink(0.09),
                chip: ink(0.04),
                chip_border: ink(0.08),
                hover: ink(0.05),
                press: ink(0.08),
                separator: ink(0.08),
                scrim: Color(0.0, 0.0, 0.0, 0.3),
                sheet: Color::hex(0xFFFFFF),
                field: ink(0.04),
                seg_on: Color::hex(0x14161A),
                seg_on_text: Color::white(1.0),
                tip_bg: Color::hex(0x14161A),
                tip_text: Color::white(1.0),
                link: Color::hex(0x1F6FEB),
                close_hover: Color::hex(0xE5484D),
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
