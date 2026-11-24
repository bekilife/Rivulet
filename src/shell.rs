//! Windows integration: one running copy, and opening .torrent files and
//! magnet links with Rivulet.
//!
//! Everything is registered per user (HKCU\Software\Classes), no admin
//! rights needed. Windows 10/11 do not let an app make itself the default
//! handler, so we register Rivulet as a capable app and send the user to
//! Settings → Default apps, where it takes one click.

use std::path::Path;

use windows::{
    Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, WPARAM},
        System::{
            DataExchange::COPYDATASTRUCT,
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
                RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW,
                RegSetValueExW,
            },
            Threading::CreateMutexW,
        },
        UI::{
            Shell::{
                ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_IS_PROTOCOL, ASSOCF_NONE, ASSOCSTR_COMMAND, AssocQueryStringW,
                SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify,
            },
            WindowsAndMessaging::{
                AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW, SMTO_ABORTIFHUNG,
                SendMessageTimeoutW,
                WM_COPYDATA,
            },
        },
    },
    core::{HSTRING, PCWSTR, PWSTR},
};

const APP: &str = "Rivulet";
const TORRENT_PROGID: &str = "Rivulet.Torrent";
const MAGNET_PROGID: &str = "Rivulet.Magnet";
const CAPABILITIES: &str = r"Software\Rivulet\Capabilities";
/// Marks a WM_COPYDATA message as "open these sources".
pub const COPYDATA_OPEN: usize = 0x5249_5654;

// ------------------------------------------------------------ single instance

/// Names unique to one data folder, so a test copy with RIVULET_DATA set does
/// not collide with the everyday one.
pub fn instance_names(data_dir: &Path) -> (String, String) {
    // The same folder can be spelled with / or \, in any case, with or
    // without a trailing slash.
    let key = data_dir.to_string_lossy().replace('/', "\\").to_lowercase();
    let key = key.trim_end_matches('\\');
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    (format!("Local\\Rivulet.{h:016x}"), format!("RivuletWindow.{h:016x}"))
}

/// True if this is the first copy for this data folder. The mutex lives as
/// long as the process.
pub fn claim_instance(mutex_name: &str) -> bool {
    unsafe {
        // The handle is never closed: Windows releases it when the process ends.
        let _ = CreateMutexW(None, false, &HSTRING::from(mutex_name));
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Hands `sources` to the copy that is already running and lets it come to
/// the front. Returns false if its window never showed up.
pub fn forward_to_running(class: &str, sources: &[String]) -> bool {
    let class = HSTRING::from(class);
    for _ in 0..50 {
        let hwnd = unsafe { FindWindowW(&class, PCWSTR::null()) };
        if let Ok(hwnd) = hwnd
            && !hwnd.is_invalid()
        {
            send_sources(hwnd, sources);
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// Asks the running copy to quit the normal way (saving its session).
/// Returns false if no copy is running.
pub fn ask_running_to_quit(class: &str) -> bool {
    let class = HSTRING::from(class);
    unsafe {
        match FindWindowW(&class, PCWSTR::null()) {
            Ok(hwnd) if !hwnd.is_invalid() => {
                let _ = PostMessageW(Some(hwnd), WM_QUIT_APP, WPARAM(0), LPARAM(0));
                true
            }
            _ => false,
        }
    }
}

/// Window message that makes Rivulet exit (sent by `rivulet --quit`).
pub const WM_QUIT_APP: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 3;

fn send_sources(hwnd: HWND, sources: &[String]) {
    let text: Vec<u16> = sources.join("\n").encode_utf16().collect();
    let cds = COPYDATASTRUCT {
        dwData: COPYDATA_OPEN,
        cbData: (text.len() * 2) as u32,
        lpData: text.as_ptr() as *mut _,
    };
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG,
            5000,
            None,
        );
    }
}

/// Reads the sources out of a WM_COPYDATA from `forward_to_running`.
///
/// # Safety
/// `lp` must be the LPARAM of a WM_COPYDATA message.
pub unsafe fn received_sources(lp: LPARAM) -> Option<Vec<String>> {
    let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
    if cds.dwData != COPYDATA_OPEN {
        return None;
    }
    let n = cds.cbData as usize / 2;
    let text = if n == 0 || cds.lpData.is_null() {
        String::new()
    } else {
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, n) })
    };
    Some(text.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect())
}

// ------------------------------------------------------------ associations

fn set(key: &str, name: Option<&str>, value: &str) -> windows::core::Result<()> {
    unsafe {
        let mut h = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut h,
            None,
        )
        .ok()?;
        let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
        let r = match name {
            Some(n) => RegSetValueExW(h, &HSTRING::from(n), None, REG_SZ, Some(bytes)),
            None => RegSetValueExW(h, PCWSTR::null(), None, REG_SZ, Some(bytes)),
        };
        let _ = RegCloseKey(h);
        r.ok()
    }
}

fn get(key: &str, name: Option<&str>) -> Option<String> {
    let mut buf = vec![0u16; 2048];
    let mut size = (buf.len() * 2) as u32;
    let name_h = name.map(HSTRING::from);
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            name_h.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if r.is_err() {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn delete_tree(key: &str) {
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
        let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(key));
    }
}

/// (values, subkeys) of a key, or None if it does not exist.
fn key_counts(key: &str) -> Option<(u32, u32)> {
    unsafe {
        let mut h = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, KEY_READ, &mut h).ok().ok()?;
        let (mut subkeys, mut values) = (0u32, 0u32);
        let r = RegQueryInfoKeyW(
            h,
            None,
            None,
            None,
            Some(&mut subkeys),
            None,
            None,
            Some(&mut values),
            None,
            None,
            None,
            None,
        );
        let _ = RegCloseKey(h);
        r.ok().ok()?;
        Some((values, subkeys))
    }
}

fn delete_if_empty(key: &str) {
    if key_counts(key) == Some((0, 0)) {
        unsafe {
            let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(key));
        }
    }
}

fn delete_value(key: &str, name: &str) {
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name));
    }
}

fn open_command(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

/// True once Rivulet has been registered as a handler (by `register`).
pub fn is_registered() -> bool {
    get(r"Software\RegisteredApplications", Some(APP)).is_some()
}

/// Registers Rivulet for .torrent files and magnet links for this user.
/// Takes over a type only where no other app had claimed it; otherwise the
/// user picks Rivulet in Settings.
pub fn register(exe: &Path) -> windows::core::Result<()> {
    let cmd = open_command(exe);
    let icon = format!("\"{}\",0", exe.display());
    let classes = r"Software\Classes";

    set(&format!(r"{classes}\{TORRENT_PROGID}"), None, "Торрент-файл")?;
    set(&format!(r"{classes}\{TORRENT_PROGID}\DefaultIcon"), None, &icon)?;
    set(&format!(r"{classes}\{TORRENT_PROGID}\shell\open\command"), None, &cmd)?;

    set(&format!(r"{classes}\{MAGNET_PROGID}"), None, "URL:Magnet-ссылка")?;
    set(&format!(r"{classes}\{MAGNET_PROGID}"), Some("URL Protocol"), "")?;
    set(&format!(r"{classes}\{MAGNET_PROGID}\DefaultIcon"), None, &icon)?;
    set(&format!(r"{classes}\{MAGNET_PROGID}\shell\open\command"), None, &cmd)?;

    set(&format!(r"{classes}\.torrent\OpenWithProgids"), Some(TORRENT_PROGID), "")?;
    set(&format!(r"{classes}\.torrent"), Some("Content Type"), "application/x-bittorrent")?;
    // Only where no app (per user or machine-wide) handles the type yet;
    // otherwise the user chooses in Settings.
    let ours = |v: &str| v.to_ascii_lowercase().contains("rivulet");
    if handler_command(".torrent", false).as_deref().is_none_or(ours) {
        set(&format!(r"{classes}\.torrent"), None, TORRENT_PROGID)?;
    }
    // The scheme key must exist for Windows to know "magnet:" at all.
    if handler_command("magnet", true).as_deref().is_none_or(ours) {
        set(&format!(r"{classes}\magnet"), None, "URL:Magnet-ссылка")?;
        set(&format!(r"{classes}\magnet"), Some("URL Protocol"), "")?;
        set(&format!(r"{classes}\magnet\DefaultIcon"), None, &icon)?;
        set(&format!(r"{classes}\magnet\shell\open\command"), None, &cmd)?;
    }

    let app_key = format!(r"{classes}\Applications\rivulet.exe");
    set(&format!(r"{app_key}\SupportedTypes"), Some(".torrent"), "")?;
    set(&format!(r"{app_key}\shell\open\command"), None, &cmd)?;
    set(&app_key, Some("FriendlyAppName"), APP)?;

    set(CAPABILITIES, Some("ApplicationName"), APP)?;
    set(CAPABILITIES, Some("ApplicationDescription"), "Лёгкий торрент-клиент")?;
    set(CAPABILITIES, Some("ApplicationIcon"), &icon)?;
    set(&format!(r"{CAPABILITIES}\FileAssociations"), Some(".torrent"), TORRENT_PROGID)?;
    set(&format!(r"{CAPABILITIES}\URLAssociations"), Some("magnet"), MAGNET_PROGID)?;
    set(r"Software\RegisteredApplications", Some(APP), CAPABILITIES)?;

    notify_shell();
    Ok(())
}

/// Removes everything `register` added. Leaves other apps' entries alone.
pub fn unregister() {
    let classes = r"Software\Classes";
    delete_tree(&format!(r"{classes}\{TORRENT_PROGID}"));
    delete_tree(&format!(r"{classes}\{MAGNET_PROGID}"));
    delete_tree(&format!(r"{classes}\Applications\rivulet.exe"));
    delete_value(&format!(r"{classes}\.torrent\OpenWithProgids"), TORRENT_PROGID);
    if get(&format!(r"{classes}\.torrent"), None).as_deref() == Some(TORRENT_PROGID) {
        delete_value(&format!(r"{classes}\.torrent"), "");
    }
    // Drop the keys register() created if nothing else lives in them now.
    delete_if_empty(&format!(r"{classes}\.torrent\OpenWithProgids"));
    if key_counts(&format!(r"{classes}\.torrent")) == Some((1, 0))
        && get(&format!(r"{classes}\.torrent"), Some("Content Type")).is_some()
    {
        delete_value(&format!(r"{classes}\.torrent"), "Content Type");
    }
    delete_if_empty(&format!(r"{classes}\.torrent"));
    if get(&format!(r"{classes}\magnet\shell\open\command"), None)
        .is_some_and(|v| v.to_ascii_lowercase().contains("rivulet"))
    {
        delete_tree(&format!(r"{classes}\magnet"));
    }
    delete_tree(r"Software\Rivulet");
    delete_value(r"Software\RegisteredApplications", APP);
    notify_shell();
}

fn notify_shell() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// The command Windows would run to open `assoc` (".torrent" or "magnet").
fn handler_command(assoc: &str, protocol: bool) -> Option<String> {
    let flags = if protocol { ASSOCF_IS_PROTOCOL } else { ASSOCF_NONE } | ASSOCF_INIT_IGNOREUNKNOWN;
    let mut buf = vec![0u16; 2048];
    let mut len = buf.len() as u32;
    unsafe {
        AssocQueryStringW(
            flags,
            ASSOCSTR_COMMAND,
            &HSTRING::from(assoc),
            PCWSTR::null(),
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
        .ok()
        .ok()?;
    }
    let len = (len as usize).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Which of .torrent / magnet currently open in this exe.
pub fn defaults(exe: &Path) -> (bool, bool) {
    let me = exe.to_string_lossy().to_lowercase();
    let is_me = |c: Option<String>| c.is_some_and(|c| c.to_lowercase().contains(&me));
    (is_me(handler_command(".torrent", false)), is_me(handler_command("magnet", true)))
}

// ------------------------------------------------------------ autostart

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn autostart_command(exe: &Path) -> String {
    format!("\"{}\" --minimized", exe.display())
}

pub fn autostart_enabled() -> bool {
    get(RUN_KEY, Some(APP)).is_some()
}

/// Starts Rivulet in the tray when the user signs in.
pub fn set_autostart(exe: &Path, on: bool) -> windows::core::Result<()> {
    if on {
        set(RUN_KEY, Some(APP), &autostart_command(exe))
    } else {
        delete_value(RUN_KEY, APP);
        Ok(())
    }
}

/// Points an existing autostart entry at `exe` if it was moved.
pub fn refresh_autostart(exe: &Path) {
    if get(RUN_KEY, Some(APP)).is_some_and(|v| v != autostart_command(exe)) {
        let _ = set(RUN_KEY, Some(APP), &autostart_command(exe));
    }
}
