//! Small Win32 helpers: file/folder pickers, clipboard, popup menus, shell.

use std::path::{Path, PathBuf};

use windows::{
    Win32::{
        Foundation::{HGLOBAL, HWND, LPARAM, POINT, WPARAM},
        Storage::FileSystem::GetDiskFreeSpaceExW,
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
            DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
            Memory::{GlobalLock, GlobalUnlock},
        },
        UI::{
            Shell::{
                Common::COMDLG_FILTERSPEC, FOLDERID_Downloads, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST,
                FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, KF_FLAG_DEFAULT,
                SHCreateItemFromParsingName, SHGetKnownFolderPath, SIGDN_FILESYSPATH, ShellExecuteW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
                MF_STRING, MF_UNCHECKED, PostMessageW, SW_SHOWNORMAL, SetForegroundWindow, TPM_RETURNCMD,
                TPM_RIGHTBUTTON, TrackPopupMenu, WM_NULL,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::wide;

pub fn downloads_folder() -> PathBuf {
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as _));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Downloads")).unwrap_or_else(|| PathBuf::from("."))
}

/// Free bytes for the current user on the volume holding `path` (or its
/// nearest existing parent).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let mut free = 0u64;
    let h = HSTRING::from(p.as_os_str());
    unsafe { GetDiskFreeSpaceExW(&h, Some(&mut free), None, None).ok()? };
    Some(free)
}

fn item_path(item: &IShellItem) -> Option<PathBuf> {
    unsafe {
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

/// "Open .torrent" dialog, several files allowed.
pub fn pick_torrents(owner: HWND) -> Vec<PathBuf> {
    unsafe {
        let Ok(dlg) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else {
            return Vec::new();
        };
        let filters = [
            COMDLG_FILTERSPEC { pszName: w!("Торрент-файлы"), pszSpec: w!("*.torrent") },
            COMDLG_FILTERSPEC { pszName: w!("Все файлы"), pszSpec: w!("*.*") },
        ];
        let _ = dlg.SetFileTypes(&filters);
        let _ = dlg.SetTitle(w!("Открыть торрент"));
        if let Ok(o) = dlg.GetOptions() {
            let _ = dlg.SetOptions(o | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
        }
        if dlg.Show(Some(owner)).is_err() {
            return Vec::new();
        }
        let Ok(items) = dlg.GetResults() else { return Vec::new() };
        let n = items.GetCount().unwrap_or(0);
        (0..n).filter_map(|i| items.GetItemAt(i).ok()).filter_map(|it| item_path(&it)).collect()
    }
}

pub fn pick_folder(owner: HWND, start: &Path) -> Option<PathBuf> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let o = dlg.GetOptions().ok()?;
        dlg.SetOptions(o | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dlg.SetTitle(w!("Куда сохранить"));
        let h = HSTRING::from(start.as_os_str());
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&h, None) {
            let _ = dlg.SetFolder(&item);
        }
        dlg.Show(Some(owner)).ok()?;
        item_path(&dlg.GetResult().ok()?)
    }
}

pub fn clipboard_text(owner: HWND) -> Option<String> {
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let text = (|| {
            let h = GetClipboardData(CF_UNICODETEXT).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let s = PCWSTR(p).to_string().ok();
            let _ = GlobalUnlock(g);
            s
        })();
        let _ = CloseClipboard();
        text
    }
}

pub fn open_in_explorer(path: &Path) {
    let h = HSTRING::from(path.as_os_str());
    unsafe {
        ShellExecuteW(None, w!("open"), &h, None, None, SW_SHOWNORMAL);
    }
}

pub enum Item<'a> {
    Cmd(usize, &'a str),
    Check(usize, &'a str, bool),
    Disabled(&'a str),
    Sep,
}

/// Shows a popup menu at the cursor and returns the chosen id (0 = none).
/// Runs a modal loop: call it with no app state borrowed.
pub fn popup(owner: HWND, items: &[Item]) -> usize {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return 0 };
        for it in items {
            add(m, it);
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(owner);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, None, owner, None).0 as usize;
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        cmd
    }
}

fn add(m: HMENU, it: &Item) {
    unsafe {
        match it {
            Item::Cmd(id, text) => {
                let t = wide(text);
                let _ = AppendMenuW(m, MF_STRING, *id, PCWSTR(t.as_ptr()));
            }
            Item::Check(id, text, on) => {
                let t = wide(text);
                let _ =
                    AppendMenuW(m, MF_STRING | if *on { MF_CHECKED } else { MF_UNCHECKED }, *id, PCWSTR(t.as_ptr()));
            }
            Item::Disabled(text) => {
                let t = wide(text);
                let _ = AppendMenuW(m, MF_STRING | MF_GRAYED, 0, PCWSTR(t.as_ptr()));
            }
            Item::Sep => {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
            }
        }
    }
}

/// Pulls torrent sources out of pasted text: magnet links, http(s) links to
/// .torrent files and bare 40-character info hashes.
pub fn sources_in_text(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|s| s.trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>'))
        .filter(|s| {
            s.starts_with("magnet:?")
                || ((s.starts_with("http://") || s.starts_with("https://"))
                    && s.to_ascii_lowercase().contains(".torrent"))
                || (s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .map(str::to_owned)
        .collect()
}
