//! Notification-area icon: tooltip with speeds, click to show the window,
//! and balloon notifications (Windows 11 shows them as regular toasts).

use windows::{
    Win32::{
        Foundation::HWND,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{LIM_LARGE, LIM_SMALL, LoadIconMetric},
            Shell::{
                NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_LARGE_ICON, NIIF_RESPECT_QUIET_TIME,
                NIIF_USER, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{HICON, WM_APP},
        },
    },
    core::PCWSTR,
};

/// Callback message for icon events.
pub const WM_TRAY: u32 = WM_APP + 2;
const ID: u32 = 1;

fn icon(large: bool) -> HICON {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        LoadIconMetric(Some(hinst.into()), PCWSTR(1 as _), if large { LIM_LARGE } else { LIM_SMALL }).unwrap_or_default()
    }
}

fn base(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ID,
        ..Default::default()
    }
}

fn copy(dst: &mut [u16], s: &str) {
    let n = dst.len() - 1;
    let mut i = 0;
    for u in s.encode_utf16().take(n) {
        dst[i] = u;
        i += 1;
    }
    dst[i] = 0;
}

pub fn add(hwnd: HWND, tip: &str) {
    let mut d = base(hwnd);
    d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = icon(false);
    copy(&mut d.szTip, tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &d);
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
    }
}

pub fn set_tip(hwnd: HWND, tip: &str) {
    let mut d = base(hwnd);
    d.uFlags = NIF_TIP | NIF_SHOWTIP;
    copy(&mut d.szTip, tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

/// A notification from the tray icon. Windows keeps it quiet during Focus
/// assist / Do not disturb.
pub fn notify(hwnd: HWND, title: &str, text: &str) {
    #[cfg(debug_assertions)]
    eprintln!("notify: {title} | {text}");
    let mut d = base(hwnd);
    d.uFlags = NIF_INFO;
    copy(&mut d.szInfoTitle, title);
    copy(&mut d.szInfo, text);
    d.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON | NIIF_RESPECT_QUIET_TIME;
    d.hBalloonIcon = icon(true);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
    }
}

pub fn remove(hwnd: HWND) {
    let d = base(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &d);
    }
}
