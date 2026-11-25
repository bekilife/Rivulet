//! Rivulet window: Win32 + Direct2D, the torrent engine runs on a tokio
//! runtime in the background and reports back through `WM_ENGINE`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod lang;
mod fmt;
#[allow(dead_code)]
mod gfx;
mod icons;
mod platform;
mod shell;
mod stats;
mod theme;
mod tray;
mod ui;

use std::{
    cell::RefCell,
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicIsize, Ordering},
    },
    time::Duration,
};

use rivulet::engine::{Draft, Engine, Profile, Status};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::{DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute},
            Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient},
        },
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
            LibraryLoader::GetModuleHandleW,
        },
        UI::{
            Controls::MARGINS,
            HiDpi::{GetDpiForSystem, GetDpiForWindow, GetSystemMetricsForDpi},
            Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK,
                VK_CONTROL, VK_DELETE, VK_ESCAPE, VK_F2, VK_RETURN, VK_SPACE,
            },
            Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP},
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

use crate::{
    gfx::{Gfx, Surface},
    lang::tt,
    platform::Item,
    theme::Theme,
    ui::{Hit, Sheet, SheetState, Ui},
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

const WM_ENGINE: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const T_REFRESH: usize = 1;
const T_SPIN: usize = 2;
const T_CARET: usize = 3;
const T_NOTICE: usize = 4;

enum Event {
    Preview {
        token: u64,
        result: Result<Draft, String>,
    },
    Added {
        token: u64,
        result: Result<(String, String), String>,
    },
    Failed(String),
    Refresh,
    /// Sources handed over by a second copy of the app.
    Open(Vec<String>),
}

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());
static HWND_RAW: AtomicIsize = AtomicIsize::new(0);

fn post(ev: Event) {
    EVENTS.lock().unwrap().push(ev);
    let raw = HWND_RAW.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_ENGINE, WPARAM(0), LPARAM(0));
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Prefs {
    precise: bool,
    /// The close button hides the window to the tray instead of quitting.
    close_to_tray: bool,
    /// Notify when a download finishes or fails.
    notify_done: bool,
    /// The "still running in the tray" hint was shown once.
    tray_hint_shown: bool,
    /// Interface language.
    lang: lang::Lang,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            precise: false,
            close_to_tray: true,
            notify_done: true,
            tray_hint_shown: false,
            lang: lang::Lang::System,
        }
    }
}

struct App {
    hwnd: HWND,
    gfx: Gfx,
    surf: Surface,
    ui: Ui,
    engine: Arc<Engine>,
    rt: tokio::runtime::Runtime,
    queue: VecDeque<String>,
    queue_total: usize,
    queue_pos: usize,
    token: u64,
    data_dir: PathBuf,
    mouse: (f32, f32),
    tracking: bool,
    prefs: Prefs,
    stats: stats::Stats,
    /// Refreshes since the statistics were last written to disk.
    stats_ticks: u32,
    /// Status of each download at the last refresh, to spot finished ones.
    last_status: std::collections::HashMap<String, Status>,
    /// Added in this session and not finished yet: small ones can finish
    /// between two refreshes without ever showing as downloading.
    fresh: std::collections::HashSet<String>,
    /// Download the last notification was about (clicking it selects it).
    notified: Option<String>,
    #[cfg(debug_assertions)]
    dump: Option<PathBuf>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app state, unless it is already borrowed (a modal loop
/// re-entered the window procedure).
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

/// Work that runs a modal loop (menus, dialogs), done with no state borrowed.
enum Action {
    None,
    AddMenu,
    OpenFiles,
    RemoveMenu,
    SettingsMenu,
    CardMenu(usize),
    PickFolder,
    ConfirmRemove,
    TrayMenu,
    WinMin,
    WinMax,
    WinClose,
    ResetStats,
}

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    lang::set(lang::Lang::System);
    let data_dir = std::env::var_os("RIVULET_DATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Rivulet")))
        .unwrap_or_else(|| PathBuf::from("Rivulet"));
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.trim().is_empty()).collect();
    // `rivulet --register` / `--unregister`: for installers and scripts.
    match args.first().map(String::as_str) {
        Some("--register") => {
            let ok = std::env::current_exe().is_ok_and(|exe| shell::register(&exe).is_ok());
            std::process::exit(if ok { 0 } else { 1 });
        }
        Some("--unregister") => {
            shell::unregister();
            return;
        }
        _ => {}
    }

    let minimized = args.iter().any(|a| a == "--minimized");
    let args: Vec<String> = args.into_iter().filter(|a| !a.starts_with("--")).collect();

    // One copy per data folder: a second launch (say, a double-clicked
    // .torrent) hands its arguments to the running one and quits.
    let (mutex_name, class_name) = shell::instance_names(&data_dir);
    // `rivulet --quit`: close the running copy cleanly (for updates).
    if std::env::args().any(|a| a == "--quit") {
        shell::ask_running_to_quit(&class_name);
        return;
    }
    if !shell::claim_instance(&mutex_name) {
        if shell::forward_to_running(&class_name, &args) {
            return;
        }
        let text = wide(tt(
            "Rivulet уже запущен, но его окно не отвечает.",
            "Rivulet is already running, but its window does not respond.",
        ));
        unsafe {
            MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Rivulet"), MB_ICONWARNING);
        }
        return;
    }
    // Keep the registered paths pointing at this exe if it was moved.
    if !cfg!(debug_assertions)
        && shell::is_registered()
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = shell::register(&exe);
    }
    if !cfg!(debug_assertions)
        && let Ok(exe) = std::env::current_exe()
    {
        shell::refresh_autostart(&exe);
    }
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
    let engine = match rt.block_on(Engine::start(&data_dir, &platform::downloads_folder())) {
        Ok(e) => Arc::new(e),
        Err(e) => {
            let text =
                wide(&tr!("Не удалось запустить торрент-движок:\n{e:#}", "Could not start the torrent engine:\n{e:#}"));
            unsafe {
                MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Rivulet"), MB_ICONERROR);
            }
            return;
        }
    };
    let prefs: Prefs =
        std::fs::read(data_dir.join("ui.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    lang::set(prefs.lang);

    let hinst = unsafe { GetModuleHandleW(None).unwrap_or_default() };
    let class_w = wide(&class_name);
    let class = PCWSTR(class_w.as_ptr());
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: class,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        hIcon: unsafe { LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).unwrap_or_default() },
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) };
    let s = unsafe { GetDpiForSystem() } as f32 / 96.0;
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Rivulet"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            (580.0 * s) as i32,
            (760.0 * s) as i32,
            None,
            None,
            Some(hinst.into()),
            None,
        )
    }
    .expect("window");
    HWND_RAW.store(hwnd.0 as isize, Ordering::Relaxed);
    let theme = Theme::current();
    apply_frame(hwnd, &theme);
    unsafe {
        DragAcceptFiles(hwnd, true);
        SetTimer(Some(hwnd), T_REFRESH, 1000, None);
    }
    tray::add(hwnd, "Rivulet");
    unsafe {
        // Recalculate the frame now that WM_NCCALCSIZE drops the caption.
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
    }

    let app = App {
        hwnd,
        gfx: Gfx::new().expect("Direct2D"),
        surf: Surface::new(),
        ui: Ui::new(theme, prefs.precise),
        prefs,
        stats: stats::Stats::load(&data_dir),
        stats_ticks: 0,
        last_status: Default::default(),
        fresh: Default::default(),
        notified: None,
        engine,
        rt,
        queue: VecDeque::new(),
        queue_total: 0,
        queue_pos: 0,
        token: 0,
        data_dir,
        mouse: (0.0, 0.0),
        tracking: false,
        #[cfg(debug_assertions)]
        dump: std::env::var_os("RIVULET_DUMP").map(PathBuf::from),
    };
    APP.with(|a| *a.borrow_mut() = Some(app));
    with(|a| {
        refresh(a);
        add_sources(a, args);
    });

    unsafe {
        if !minimized {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    HWND_RAW.store(0, Ordering::Relaxed);
    if let Some(mut app) = APP.with(|a| a.borrow_mut().take()) {
        app.stats.save();
        let engine = app.engine.clone();
        app.rt.block_on(async {
            let _ = tokio::time::timeout(Duration::from_secs(3), engine.stop()).await;
        });
        app.rt.shutdown_timeout(Duration::from_secs(1));
    }
}

/// Mica behind the whole window, dark caption in dark mode.
fn apply_frame(hwnd: HWND, theme: &Theme) {
    unsafe {
        let dark: i32 = theme.dark as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), &dark as *const i32 as _, 4);
        let mica: i32 = 2; // DWMSBT_MAINWINDOW
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(38), &mica as *const i32 as _, 4);
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        let round: i32 = 2; // DWMWCP_ROUND
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(33), &round as *const i32 as _, 4);
    }
}

fn scale_of(hwnd: HWND) -> f32 {
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 { 1.0 } else { d as f32 / 96.0 }
}

fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn save_prefs(a: &mut App) {
    a.prefs.precise = a.ui.precise;
    if let Ok(b) = serde_json::to_vec_pretty(&a.prefs) {
        let _ = std::fs::write(a.data_dir.join("ui.json"), b);
    }
}

fn refresh(a: &mut App) {
    a.ui.snaps = a.engine.snapshots();
    let hashes: std::collections::HashSet<String> = a.ui.snaps.iter().map(|s| s.info_hash.clone()).collect();
    a.ui.selected.retain(|h| hashes.contains(h));
    a.ui.expanded.retain(|h| hashes.contains(h));
    a.stats.record(&a.ui.snaps);
    a.stats_ticks += 1;
    if a.stats_ticks >= 30 {
        a.stats_ticks = 0;
        a.stats.save();
    }
    announce_changes(a);
    tray::set_tip(a.hwnd, &tray_tip(&a.ui.snaps));
    invalidate(a.hwnd);
}

/// Notifies about downloads that finished or failed since the last refresh.
/// A download only counts as finished if we saw it downloading, so restoring
/// the session or re-checking files stays quiet.
fn announce_changes(a: &mut App) {
    let mut done: Vec<(String, String)> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for s in &a.ui.snaps {
        let title = s.alias.clone().unwrap_or_else(|| s.name.clone());
        let prev = a.last_status.get(&s.info_hash).copied();
        let finished = matches!(s.status, Status::Seeding | Status::Finished);
        if finished && a.fresh.remove(&s.info_hash) && prev != Some(Status::Downloading) {
            done.push((s.info_hash.clone(), title));
            continue;
        }
        match (prev, s.status) {
            (Some(Status::Downloading), Status::Seeding | Status::Finished) => done.push((s.info_hash.clone(), title)),
            (Some(prev), Status::Error) if prev != Status::Error => failed.push((s.info_hash.clone(), title)),
            _ => {}
        }
    }
    a.last_status = a.ui.snaps.iter().map(|s| (s.info_hash.clone(), s.status)).collect();
    if !a.prefs.notify_done {
        return;
    }
    let (title, text, hash) = match (done.as_slice(), failed.as_slice()) {
        ([], []) => return,
        ([(h, name)], []) => (tt("Загрузка завершена", "Download complete").to_string(), name.clone(), Some(h.clone())),
        (d, []) => (
            tr!("Завершено загрузок: {}", "Downloads complete: {}", d.len()),
            d.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", "),
            None,
        ),
        (_, [(h, name), ..]) => (tt("Ошибка загрузки", "Download failed").to_string(), name.clone(), Some(h.clone())),
    };
    a.notified = hash;
    tray::notify(a.hwnd, &title, &text);
}

fn tray_tip(snaps: &[rivulet::engine::Snapshot]) -> String {
    let active: Vec<_> = snaps.iter().filter(|s| s.status == Status::Downloading).collect();
    if active.is_empty() {
        return if snaps.is_empty() {
            "Rivulet".into()
        } else {
            tt("Rivulet — активных загрузок нет", "Rivulet — no active downloads").into()
        };
    }
    let down: u64 = active.iter().map(|s| s.down_bps).sum();
    let up: u64 = snaps.iter().map(|s| s.up_bps).sum();
    tr!(
        "Rivulet — загрузок: {}\n↓ {}  ↑ {}",
        "Rivulet — downloads: {}\n↓ {}  ↑ {}",
        active.len(),
        fmt::speed(down, false),
        fmt::speed(up, false)
    )
}

/// Brings the window back from the tray, the taskbar or behind other windows.
fn show_window(hwnd: HWND) {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

fn notify(a: &mut App, text: impl Into<String>) {
    a.ui.notify(text);
    unsafe {
        SetTimer(Some(a.hwnd), T_NOTICE, 4100, None);
    }
    invalidate(a.hwnd);
}

/// Runs an engine call in the background; failures show as a notice.
fn spawn_op<F>(a: &App, fut: F)
where
    F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    a.rt.spawn(async move {
        match fut.await {
            Ok(()) => post(Event::Refresh),
            Err(e) => post(Event::Failed(russian_error(&e.to_string()))),
        }
    });
}

fn russian_error(e: &str) -> String {
    if e.contains("already in the list") || e.contains("already managed") {
        tt("Этот торрент уже есть в списке", "This torrent is already in the list").into()
    } else if e.contains("no files selected") {
        tt("Не выбрано ни одного файла", "No files selected").into()
    } else {
        tr!("Ошибка: {e}", "Error: {e}")
    }
}

// ------------------------------------------------------------------ sources

fn add_sources(a: &mut App, sources: Vec<String>) {
    if sources.is_empty() {
        return;
    }
    if a.ui.sheet.is_none() && a.queue.is_empty() {
        a.queue_total = 0;
        a.queue_pos = 0;
    }
    a.queue_total += sources.len();
    a.queue.extend(sources);
    open_next(a);
}

fn open_next(a: &mut App) {
    if a.ui.sheet.is_some() {
        return;
    }
    let Some(source) = a.queue.pop_front() else {
        a.queue_total = 0;
        a.queue_pos = 0;
        return;
    };
    a.queue_pos += 1;
    a.token += 1;
    let token = a.token;
    let folder = a.engine.download_dir().unwrap_or_else(platform::downloads_folder);
    a.ui.sheet = Some(Sheet {
        source: source.clone(),
        state: SheetState::Loading,
        token,
        alias: String::new(),
        alias_focus: false,
        checked: Vec::new(),
        free: platform::free_space(&folder),
        folder,
        file_scroll: 0.0,
        position: (a.queue_pos, a.queue_total),
        busy: false,
    });
    a.ui.hover = None;
    unsafe {
        SetTimer(Some(a.hwnd), T_SPIN, 33, None);
    }
    let engine = a.engine.clone();
    a.rt.spawn(async move {
        let result = match tokio::time::timeout(Duration::from_secs(120), engine.preview(&source)).await {
            Ok(Ok(d)) => Ok(d),
            Ok(Err(e)) => Err(if e.to_string().contains("already in the list") {
                tt("Этот торрент уже есть в списке.", "This torrent is already in the list.").to_string()
            } else {
                tr!("Не удалось открыть торрент.\n{e:#}", "Could not open the torrent.\n{e:#}")
            }),
            Err(_) => Err(tt(
                "За 2 минуты не удалось получить список файлов. Возможно, сейчас никто не раздаёт этот торрент.",
                "Could not get the file list in 2 minutes. Maybe nobody is sharing this torrent right now.",
            )
            .to_string()),
        };
        post(Event::Preview { token, result });
    });
    invalidate(a.hwnd);
}

fn close_sheet(a: &mut App) {
    a.ui.sheet = None;
    a.ui.hover = None;
    unsafe {
        let _ = KillTimer(Some(a.hwnd), T_SPIN);
        let _ = KillTimer(Some(a.hwnd), T_CARET);
    }
    open_next(a);
    invalidate(a.hwnd);
}

fn start_download(a: &mut App) {
    let Some(sheet) = a.ui.sheet.as_mut() else { return };
    if !sheet.can_start() {
        return;
    }
    let SheetState::Ready(draft) = &sheet.state else { return };
    sheet.busy = true;
    let draft = draft.clone();
    let files = if sheet.checked.iter().all(|c| *c) {
        None
    } else {
        Some(sheet.checked.iter().enumerate().filter(|(_, c)| **c).map(|(i, _)| i).collect())
    };
    let folder = sheet.folder.clone();
    let alias = Some(sheet.alias.clone());
    let token = sheet.token;
    let engine = a.engine.clone();
    a.rt.spawn(async move {
        let result = engine
            .add(&draft, files, Some(&folder), alias)
            .await
            .map(|_| {
                let _ = engine.set_download_dir(&folder);
                (draft.name.clone(), draft.info_hash.clone())
            })
            .map_err(|e| russian_error(&e.to_string()));
        post(Event::Added { token, result });
    });
    invalidate(a.hwnd);
}

fn handle_events(a: &mut App) {
    let events: Vec<Event> = std::mem::take(&mut *EVENTS.lock().unwrap());
    for ev in events {
        match ev {
            Event::Preview { token, result } => {
                let Some(sheet) = a.ui.sheet.as_mut().filter(|s| s.token == token) else { continue };
                sheet.state = match result {
                    Ok(d) => {
                        sheet.checked = vec![true; d.files.len()];
                        SheetState::Ready(d)
                    }
                    Err(e) => SheetState::Failed(e),
                };
                unsafe {
                    let _ = KillTimer(Some(a.hwnd), T_SPIN);
                }
            }
            Event::Added { token, result } => {
                if !a.ui.sheet.as_ref().is_some_and(|s| s.token == token) {
                    continue;
                }
                match result {
                    Ok((name, hash)) => {
                        a.fresh.insert(hash);
                        close_sheet(a);
                        refresh(a);
                        notify(a, tr!("Добавлено: {name}", "Added: {name}"));
                    }
                    Err(e) => {
                        if let Some(s) = a.ui.sheet.as_mut() {
                            s.busy = false;
                        }
                        notify(a, e);
                    }
                }
            }
            Event::Failed(e) => notify(a, e),
            Event::Refresh => refresh(a),
            Event::Open(sources) => {
                show_window(a.hwnd);
                add_sources(a, sources);
            }
        }
    }
    invalidate(a.hwnd);
}

// ------------------------------------------------------------------ input

fn ctrl_down() -> bool {
    unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 }
}

fn click(a: &mut App, hit: Hit) -> Action {
    if a.ui.rename.is_some() {
        match hit {
            Hit::RenameOk => save_rename(a),
            Hit::RenameCancel => a.ui.rename = None,
            _ => {}
        }
        invalidate(a.hwnd);
        return Action::None;
    }
    if !matches!(hit, Hit::SearchField | Hit::SearchClear | Hit::Search) {
        a.ui.search_focus = false;
    }
    match hit {
        Hit::Search | Hit::SearchField => {
            open_search(a);
            return Action::None;
        }
        Hit::SearchClear => {
            close_search(a);
            return Action::None;
        }
        _ => {}
    }
    let ui = &mut a.ui;
    if let Some(sheet) = ui.sheet.as_mut() {
        if hit != Hit::SheetAlias {
            sheet.alias_focus = false;
        }
        match hit {
            Hit::SheetFile(k) => {
                if let Some(c) = sheet.checked.get_mut(k) {
                    *c = !*c;
                }
            }
            Hit::SheetAll => {
                let all = sheet.checked.iter().all(|c| *c);
                sheet.checked.iter_mut().for_each(|c| *c = !all);
            }
            Hit::SheetAlias => {
                sheet.alias_focus = true;
                ui.caret_on = true;
                unsafe {
                    SetTimer(Some(a.hwnd), T_CARET, 530, None);
                }
            }
            Hit::SheetFolder => return Action::PickFolder,
            Hit::SheetCancel => close_sheet(a),
            Hit::SheetOk => start_download(a),
            _ => {}
        }
        invalidate(a.hwnd);
        return Action::None;
    }
    match hit {
        Hit::WinMin => return Action::WinMin,
        Hit::WinMax => return Action::WinMax,
        Hit::WinClose => return Action::WinClose,
        Hit::Stats | Hit::Summary => {
            ui.view = ui::View::Stats;
            ui.search_focus = false;
        }
        Hit::Back => ui.view = ui::View::List,
        Hit::Period(k) => ui.period = k,
        Hit::StatsReset => return Action::ResetStats,
        Hit::Add => return Action::AddMenu,
        Hit::Remove => return Action::RemoveMenu,
        Hit::Settings => return Action::SettingsMenu,
        Hit::Toggle => toggle_selected(a),
        Hit::Card(i) => {
            let hash = ui.snaps[i].info_hash.clone();
            if ctrl_down() {
                if !ui.selected.remove(&hash) {
                    ui.selected.insert(hash);
                }
            } else if ui.selected.len() == 1 && ui.selected.contains(&hash) {
                ui.selected.clear();
            } else {
                ui.selected.clear();
                ui.selected.insert(hash);
            }
        }
        Hit::Expand(i) => {
            let hash = ui.snaps[i].info_hash.clone();
            if !ui.expanded.remove(&hash) {
                ui.expanded.insert(hash);
            }
        }
        _ => {}
    }
    invalidate(a.hwnd);
    Action::None
}

fn start_caret(a: &mut App) {
    a.ui.caret_on = true;
    unsafe {
        SetTimer(Some(a.hwnd), T_CARET, 530, None);
    }
}

fn open_search(a: &mut App) {
    if a.ui.search.is_none() {
        a.ui.search = Some(String::new());
    }
    a.ui.search_focus = true;
    a.ui.scroll = 0.0;
    start_caret(a);
    invalidate(a.hwnd);
}

fn close_search(a: &mut App) {
    a.ui.search = None;
    a.ui.search_focus = false;
    invalidate(a.hwnd);
}

fn start_rename(a: &mut App, hash: &str) {
    let Some(s) = a.ui.snaps.iter().find(|s| s.info_hash == hash) else { return };
    a.ui.rename = Some(ui::Rename {
        hash: s.info_hash.clone(),
        original: s.name.clone(),
        text: s.alias.clone().unwrap_or_default(),
    });
    a.ui.search_focus = false;
    a.ui.hover = None;
    start_caret(a);
    invalidate(a.hwnd);
}

fn save_rename(a: &mut App) {
    let Some(r) = a.ui.rename.take() else { return };
    match a.engine.set_alias(&r.hash, &r.text) {
        Ok(()) => {
            refresh(a);
            notify(
                a,
                if r.text.trim().is_empty() {
                    tt("Название из торрента возвращено", "Name from the torrent restored")
                } else {
                    tt("Переименовано", "Renamed")
                },
            );
        }
        Err(e) => notify(a, tr!("Не удалось сохранить название: {e}", "Could not save the name: {e}")),
    }
}

fn toggle_selected(a: &mut App) {
    let pause = a.ui.toggle_pauses();
    let ids: Vec<(usize, Status)> = a.ui.selected_snaps().iter().map(|s| (s.id, s.status)).collect();
    for (id, status) in ids {
        let engine = a.engine.clone();
        if pause && matches!(status, Status::Downloading | Status::Seeding | Status::Checking) {
            spawn_op(a, async move { engine.pause(id).await });
        } else if !pause && matches!(status, Status::Paused | Status::Finished | Status::Error) {
            spawn_op(a, async move { engine.resume(id).await });
        }
    }
}

fn remove(a: &mut App, hashes: Vec<String>, delete_files: bool) {
    let ids: Vec<usize> = a.ui.snaps.iter().filter(|s| hashes.contains(&s.info_hash)).map(|s| s.id).collect();
    for id in ids {
        let engine = a.engine.clone();
        spawn_op(a, async move { engine.remove(id, delete_files).await });
    }
    for h in &hashes {
        a.ui.selected.remove(h);
    }
}

fn confirm(hwnd: HWND, text: &str) -> bool {
    let t = wide(text);
    unsafe { MessageBoxW(Some(hwnd), PCWSTR(t.as_ptr()), w!("Rivulet"), MB_YESNO | MB_ICONWARNING) == IDYES }
}

/// Runs menus and dialogs. Called with no state borrowed.
fn run_action(hwnd: HWND, action: Action) {
    match action {
        Action::None => {}
        Action::AddMenu => {
            let cmd = platform::popup(
                hwnd,
                &[
                    Item::Cmd(1, tt("Открыть .torrent-файл…\tCtrl+O", "Open .torrent file…\tCtrl+O")),
                    Item::Cmd(2, tt("Вставить magnet-ссылку\tCtrl+V", "Paste magnet link\tCtrl+V")),
                ],
            );
            match cmd {
                1 => run_action(hwnd, Action::OpenFiles),
                2 => paste(hwnd),
                _ => {}
            }
        }
        Action::OpenFiles => {
            let files = platform::pick_torrents(hwnd);
            with(|a| add_sources(a, files.iter().map(|p| p.to_string_lossy().into_owned()).collect()));
        }
        Action::RemoveMenu => {
            let n = with(|a| a.ui.selected.len()).unwrap_or(0);
            if n == 0 {
                return;
            }
            let cmd = platform::popup(
                hwnd,
                &[
                    Item::Cmd(1, tt("Убрать из списка", "Remove from list")),
                    Item::Cmd(2, tt("Убрать и удалить файлы…", "Remove and delete files…")),
                ],
            );
            let hashes = with(|a| a.ui.selected.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
            match cmd {
                1 => {
                    with(|a| remove(a, hashes, false));
                }
                2 if confirm(
                    hwnd,
                    &tr!(
                        "Удалить скачанные файлы ({})? Это нельзя отменить.",
                        "Delete the downloaded files ({})? This cannot be undone.",
                        fmt::files(n)
                    ),
                ) =>
                {
                    with(|a| remove(a, hashes, true));
                }
                _ => {}
            }
        }
        Action::ConfirmRemove => {
            let hashes = with(|a| a.ui.selected.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
            if !hashes.is_empty()
                && confirm(
                    hwnd,
                    &tr!(
                        "Убрать из списка: {}? Файлы останутся на диске.",
                        "Remove {} from the list? The files stay on disk.",
                        hashes.len()
                    ),
                )
            {
                with(|a| remove(a, hashes, false));
            }
        }
        Action::SettingsMenu => {
            let Some((profile, precise, folder, close_to_tray, notify_done, ui_lang)) = with(|a| {
                (
                    a.engine.profile(),
                    a.ui.precise,
                    a.engine.download_dir().unwrap_or_else(platform::downloads_folder),
                    a.prefs.close_to_tray,
                    a.prefs.notify_done,
                    a.prefs.lang,
                )
            }) else {
                return;
            };
            let folder_label = tr!("Сейчас: {}", "Now: {}", folder.display());
            let exe = std::env::current_exe().unwrap_or_default();
            let (torrent_ok, magnet_ok) = shell::defaults(&exe);
            let registered = shell::is_registered();
            let assoc_label = match (torrent_ok, magnet_ok) {
                (true, true) => tt("Открывать .torrent и magnet в Rivulet ✓", "Open .torrent and magnet in Rivulet ✓"),
                (true, false) => tt("Открывать magnet-ссылки в Rivulet…", "Open magnet links in Rivulet…"),
                (false, true) => tt("Открывать .torrent-файлы в Rivulet…", "Open .torrent files in Rivulet…"),
                (false, false) => tt("Открывать .torrent и magnet в Rivulet…", "Open .torrent and magnet in Rivulet…"),
            };
            let cmd = platform::popup(
                hwnd,
                &[
                    Item::Disabled(tt("Режим работы", "Mode")),
                    Item::Check(
                        10,
                        tt("Спокойный — до 24 пиров, до 5 МБ/с", "Calm — up to 24 peers and 5 MB/s"),
                        profile == Profile::Calm,
                    ),
                    Item::Check(
                        11,
                        tt("Обычный — без ограничения скорости", "Steady — no speed cap"),
                        profile == Profile::Steady,
                    ),
                    Item::Check(
                        12,
                        tt("Максимум — всё, что позволяет сеть", "Surge — all your connection can take"),
                        profile == Profile::Surge,
                    ),
                    Item::Sep,
                    Item::Check(20, tt("Подробные цифры", "Precise numbers"), precise),
                    Item::Sep,
                    Item::Cmd(21, tt("Папка для загрузок…", "Download folder…")),
                    Item::Disabled(&folder_label),
                    Item::Cmd(22, tt("Открыть папку загрузок", "Open download folder")),
                    Item::Sep,
                    Item::Cmd(23, assoc_label),
                    if registered {
                        Item::Cmd(24, tt("Отвязать .torrent и magnet", "Stop opening .torrent and magnet"))
                    } else {
                        Item::Sep
                    },
                    Item::Sep,
                    Item::Check(30, tt("Запускать вместе с Windows", "Start with Windows"), shell::autostart_enabled()),
                    Item::Check(31, tt("При закрытии сворачивать в трей", "Close to tray"), close_to_tray),
                    Item::Check(
                        32,
                        tt("Уведомлять о завершении загрузок", "Notify when downloads finish"),
                        notify_done,
                    ),
                    Item::Sep,
                    Item::Disabled("Язык / Language"),
                    Item::Check(40, tt("Как в Windows", "Same as Windows"), ui_lang == lang::Lang::System),
                    Item::Check(41, "English", ui_lang == lang::Lang::En),
                    Item::Check(42, "Русский", ui_lang == lang::Lang::Ru),
                ],
            );
            match cmd {
                10..=12 => {
                    let p = [Profile::Calm, Profile::Steady, Profile::Surge][cmd - 10];
                    with(|a| {
                        let _ = a.engine.set_profile(p);
                        let label =
                            [tt("Спокойный", "Calm"), tt("Обычный", "Steady"), tt("Максимум", "Surge")][cmd - 10];
                        notify(a, tr!("Режим: {label}", "Mode: {label}"));
                    });
                }
                20 => {
                    with(|a| {
                        a.ui.precise = !a.ui.precise;
                        save_prefs(a);
                        invalidate(a.hwnd);
                    });
                }
                21 => {
                    if let Some(dir) = platform::pick_folder(hwnd, &folder) {
                        with(|a| {
                            let _ = a.engine.set_download_dir(&dir);
                            notify(a, tr!("Новые загрузки — в {}", "New downloads go to {}", dir.display()));
                        });
                    }
                }
                22 => platform::open_in_explorer(&folder),
                23 => associate(hwnd, &exe),
                24 => {
                    shell::unregister();
                    with(|a| {
                        notify(
                            a,
                            tt(
                                "Rivulet больше не открывает .torrent и magnet",
                                "Rivulet no longer opens .torrent and magnet",
                            ),
                        )
                    });
                }
                30 => {
                    let on = !shell::autostart_enabled();
                    let r = shell::set_autostart(&exe, on);
                    with(|a| {
                        notify(
                            a,
                            match (r, on) {
                                (Err(e), _) => tr!(
                                    "Не удалось изменить автозапуск: {e}",
                                    "Could not change start with Windows: {e}"
                                ),
                                (Ok(()), true) => tt(
                                    "Rivulet будет запускаться в трее вместе с Windows",
                                    "Rivulet will start in the tray with Windows",
                                )
                                .into(),
                                (Ok(()), false) => tt("Автозапуск выключен", "Start with Windows is off").into(),
                            },
                        )
                    });
                }
                31 | 32 => {
                    with(|a| {
                        if cmd == 31 {
                            a.prefs.close_to_tray = !a.prefs.close_to_tray;
                        } else {
                            a.prefs.notify_done = !a.prefs.notify_done;
                        }
                        save_prefs(a);
                    });
                }
                40..=42 => {
                    let l = [lang::Lang::System, lang::Lang::En, lang::Lang::Ru][cmd - 40];
                    lang::set(l);
                    with(|a| {
                        a.prefs.lang = l;
                        save_prefs(a);
                        refresh(a);
                    });
                }
                _ => {}
            }
        }
        Action::WinMin => unsafe {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        },
        Action::WinMax => unsafe {
            let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
        },
        Action::WinClose => unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        Action::ResetStats => {
            if confirm(
                hwnd,
                tt(
                    "Сбросить статистику? Счёт начнётся заново с сегодняшнего дня.",
                    "Reset statistics? Counting starts again from today.",
                ),
            ) {
                with(|a| {
                    a.stats.reset();
                    notify(a, tt("Статистика сброшена", "Statistics reset"));
                });
            }
        }
        Action::TrayMenu => {
            let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
            let cmd = platform::popup(
                hwnd,
                &[
                    Item::Cmd(
                        1,
                        if visible {
                            tt("Скрыть Rivulet", "Hide Rivulet")
                        } else {
                            tt("Открыть Rivulet", "Open Rivulet")
                        },
                    ),
                    Item::Cmd(2, tt("Открыть .torrent-файл…", "Open .torrent file…")),
                    Item::Cmd(3, tt("Вставить magnet-ссылку", "Paste magnet link")),
                    Item::Sep,
                    Item::Cmd(9, tt("Выход", "Exit")),
                ],
            );
            match cmd {
                1 if visible => unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                },
                1 => show_window(hwnd),
                2 => {
                    show_window(hwnd);
                    run_action(hwnd, Action::OpenFiles);
                }
                3 => {
                    show_window(hwnd);
                    paste(hwnd);
                }
                9 => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                _ => {}
            }
        }
        Action::CardMenu(i) => {
            let Some(s) = with(|a| a.ui.snaps.get(i).cloned()).flatten() else { return };
            let expanded = with(|a| a.ui.expanded.contains(&s.info_hash)).unwrap_or(false);
            let running = matches!(s.status, Status::Downloading | Status::Seeding | Status::Checking);
            let cmd = platform::popup(
                hwnd,
                &[
                    Item::Cmd(1, if running { tt("Пауза", "Pause") } else { tt("Продолжить", "Resume") }),
                    Item::Cmd(2, tt("Открыть папку", "Open folder")),
                    Item::Cmd(3, if expanded { tt("Свернуть", "Collapse") } else { tt("Подробнее", "Details") }),
                    Item::Cmd(6, tt("Переименовать…\tF2", "Rename…\tF2")),
                    Item::Sep,
                    Item::Cmd(4, tt("Убрать из списка", "Remove from list")),
                    Item::Cmd(5, tt("Убрать и удалить файлы…", "Remove and delete files…")),
                ],
            );
            match cmd {
                1 => {
                    with(|a| {
                        let engine = a.engine.clone();
                        let id = s.id;
                        if running {
                            spawn_op(a, async move { engine.pause(id).await });
                        } else {
                            spawn_op(a, async move { engine.resume(id).await });
                        }
                    });
                }
                2 => platform::open_in_explorer(&s.folder),
                6 => {
                    with(|a| start_rename(a, &s.info_hash));
                }
                3 => {
                    with(|a| {
                        if !a.ui.expanded.remove(&s.info_hash) {
                            a.ui.expanded.insert(s.info_hash.clone());
                        }
                        invalidate(a.hwnd);
                    });
                }
                4 => {
                    with(|a| remove(a, vec![s.info_hash.clone()], false));
                }
                5 => {
                    let title = s.alias.clone().unwrap_or(s.name.clone());
                    if confirm(
                        hwnd,
                        &tr!(
                            "Удалить скачанные файлы «{title}»? Это нельзя отменить.",
                            "Delete the downloaded files of “{title}”? This cannot be undone."
                        ),
                    ) {
                        with(|a| remove(a, vec![s.info_hash.clone()], true));
                    }
                }
                _ => {}
            }
        }
        Action::PickFolder => {
            let Some(start) = with(|a| a.ui.sheet.as_ref().map(|s| s.folder.clone())).flatten() else { return };
            if let Some(dir) = platform::pick_folder(hwnd, &start) {
                with(|a| {
                    if let Some(s) = a.ui.sheet.as_mut() {
                        s.free = platform::free_space(&dir);
                        s.folder = dir;
                    }
                    invalidate(a.hwnd);
                });
            }
        }
    }
}

/// Registers Rivulet for .torrent and magnet, then, if Windows still sends
/// them elsewhere, opens Settings → Default apps on Rivulet's page.
fn associate(hwnd: HWND, exe: &std::path::Path) {
    if let Err(e) = shell::register(exe) {
        with(|a| notify(a, tr!("Не удалось записать в реестр: {e}", "Could not write to the registry: {e}")));
        return;
    }
    let (torrent_ok, magnet_ok) = shell::defaults(exe);
    if torrent_ok && magnet_ok {
        with(|a| {
            notify(
                a,
                tt("Готово: .torrent и magnet открываются в Rivulet", "Done: .torrent and magnet now open in Rivulet"),
            )
        });
        return;
    }
    let ok = confirm_info(
        hwnd,
        tt(
            "Windows разрешает выбрать приложение по умолчанию только вам.

Сейчас откроются «Приложения по умолчанию».          Выберите Rivulet для .torrent и для MAGNET.",
            "Windows lets only you choose default apps.\n\nDefault apps will open now. Choose Rivulet for .torrent and for MAGNET.",
        ),
    );
    if ok {
        platform::open_in_explorer(std::path::Path::new("ms-settings:defaultapps?registeredAppUser=Rivulet"));
    }
}

fn confirm_info(hwnd: HWND, text: &str) -> bool {
    let t = wide(text);
    unsafe { MessageBoxW(Some(hwnd), PCWSTR(t.as_ptr()), w!("Rivulet"), MB_OKCANCEL | MB_ICONINFORMATION) == IDOK }
}

fn paste(hwnd: HWND) {
    let text = platform::clipboard_text(hwnd).unwrap_or_default();
    let sources = platform::sources_in_text(&text);
    with(|a| {
        if sources.is_empty() {
            notify(a, tt("В буфере обмена нет magnet-ссылки", "No magnet link in the clipboard"));
        } else {
            add_sources(a, sources);
        }
    });
}

/// Height of the resize border Windows would draw at the top.
fn frame_thickness(hwnd: HWND) -> i32 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    }
}

fn taskbar_created() -> u32 {
    static MSG: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

fn logical(hwnd: HWND, lp: LPARAM) -> (f32, f32) {
    let s = scale_of(hwnd);
    let x = (lp.0 & 0xFFFF) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
    (x / s, y / s)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                with(|a| {
                    let mut rc = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rc);
                    let (pw, ph) = (rc.right - rc.left, rc.bottom - rc.top);
                    let s = scale_of(hwnd);
                    let (lw, lh) = (pw as f32 / s, ph as f32 / s);
                    a.ui.maximized = IsZoomed(hwnd).as_bool();
                    let App { gfx, surf, ui, stats, .. } = a;
                    gfx.draw(surf, pw, ph, s, |g| ui.paint(g, lw, lh, stats));
                    surf.blit(dc);
                    #[cfg(debug_assertions)]
                    if let Some(dir) = &a.dump {
                        a.surf.dump(&dir.join("frame.bmp"));
                    }
                });
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_TIMER => {
                match wp.0 {
                    T_REFRESH => {
                        with(refresh);
                    }
                    T_SPIN => {
                        with(|a| {
                            a.ui.spin = (a.ui.spin + 10.0) % 360.0;
                            invalidate(hwnd);
                        });
                    }
                    T_CARET => {
                        with(|a| {
                            a.ui.caret_on = !a.ui.caret_on;
                            invalidate(hwnd);
                        });
                    }
                    T_NOTICE => {
                        let _ = KillTimer(Some(hwnd), T_NOTICE);
                        invalidate(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let hide = with(|a| {
                    if !a.prefs.close_to_tray {
                        return false;
                    }
                    if !a.prefs.tray_hint_shown {
                        a.prefs.tray_hint_shown = true;
                        save_prefs(a);
                        tray::notify(
                            hwnd,
                            tt("Rivulet продолжает работать", "Rivulet is still running"),
                            tt("Загрузки идут в фоне. Значок — в области уведомлений, выход — в его меню.", "Downloads continue in the background. The icon is in the notification area; exit from its menu."),
                        );
                    }
                    true
                })
                .unwrap_or(true);
                if hide {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                } else {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            tray::WM_TRAY => {
                const NIN_SELECT: u32 = WM_USER;
                const NIN_KEYSELECT: u32 = WM_USER + 1;
                const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
                match (lp.0 & 0xFFFF) as u32 {
                    WM_LBUTTONUP | NIN_SELECT | NIN_KEYSELECT => {
                        if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() && GetForegroundWindow() == hwnd
                        {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        } else {
                            show_window(hwnd);
                        }
                    }
                    WM_CONTEXTMENU | WM_RBUTTONUP => run_action(hwnd, Action::TrayMenu),
                    NIN_BALLOONUSERCLICK => {
                        show_window(hwnd);
                        with(|a| {
                            if let Some(h) = a.notified.take() {
                                a.ui.selected.clear();
                                a.ui.selected.insert(h);
                                invalidate(hwnd);
                            }
                        });
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            m if m == taskbar_created() => {
                // Explorer restarted: the tray icon has to be added again.
                tray::add(hwnd, "Rivulet");
                with(refresh);
                LRESULT(0)
            }
            shell::WM_QUIT_APP => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_COPYDATA => {
                // From a second copy of the app; handled via the event queue
                // so it also works while a menu or dialog is open.
                match shell::received_sources(lp) {
                    Some(sources) => {
                        post(Event::Open(sources));
                        LRESULT(1)
                    }
                    None => LRESULT(0),
                }
            }
            WM_ENGINE => {
                with(handle_events);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = logical(hwnd, lp);
                with(|a| {
                    a.mouse = (x, y);
                    if !a.tracking {
                        let mut t = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut t);
                        a.tracking = true;
                    }
                    let h = a.ui.hit(x, y);
                    if h != a.ui.hover {
                        a.ui.hover = h;
                        invalidate(hwnd);
                    }
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with(|a| {
                    a.tracking = false;
                    a.ui.hover = None;
                    invalidate(hwnd);
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let (x, y) = logical(hwnd, lp);
                let action = with(|a| {
                    let h = a.ui.hit(x, y);
                    // Double-click on a card opens its details.
                    if msg == WM_LBUTTONDBLCLK
                        && let Some(Hit::Card(i)) = h
                    {
                        return click(a, Hit::Expand(i));
                    }
                    a.ui.pressed = h.filter(|h| h.clickable());
                    if a.ui.pressed.is_some() {
                        SetCapture(hwnd);
                    } else if let Some(s) = a.ui.sheet.as_mut() {
                        s.alias_focus = false;
                    } else {
                        a.ui.search_focus = false;
                    }
                    invalidate(hwnd);
                    Action::None
                });
                if let Some(action) = action {
                    run_action(hwnd, action);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = logical(hwnd, lp);
                let _ = ReleaseCapture();
                let action = with(|a| {
                    let pressed = a.ui.pressed.take();
                    let h = a.ui.hit(x, y);
                    invalidate(hwnd);
                    match (pressed, h) {
                        (Some(p), Some(h)) if p == h => click(a, h),
                        _ => Action::None,
                    }
                });
                if let Some(action) = action {
                    run_action(hwnd, action);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let (x, y) = logical(hwnd, lp);
                let action = with(|a| match a.ui.hit(x, y) {
                    Some(Hit::Card(i) | Hit::Expand(i)) if a.ui.sheet.is_none() => Action::CardMenu(i),
                    _ => Action::None,
                });
                if let Some(action) = action {
                    run_action(hwnd, action);
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0 * 64.0;
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                let _ = ScreenToClient(hwnd, &mut pt);
                let s = scale_of(hwnd);
                with(|a| {
                    a.ui.wheel(pt.x as f32 / s, pt.y as f32 / s, delta);
                    let (mx, my) = a.mouse;
                    a.ui.hover = a.ui.hit(mx, my);
                    invalidate(hwnd);
                });
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let ctrl = ctrl_down();
                let key = wp.0 as u16;
                let is = |k: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| key == k.0;
                let action = with(|a| {
                    // Editing keys for whichever text field has focus.
                    if let Some(text) = a.ui.focused_text() {
                        if is(VK_BACK) {
                            if ctrl {
                                let keep = text.trim_end().rfind(' ').map_or(0, |i| i + 1);
                                text.truncate(keep);
                            } else {
                                text.pop();
                            }
                        } else if key == b'V' as u16
                            && ctrl
                            && let Some(t) = platform::clipboard_text(hwnd)
                        {
                            text.push_str(t.lines().next().unwrap_or("").trim());
                        }
                        a.ui.caret_on = true;
                    }
                    if a.ui.sheet.is_some() {
                        if is(VK_ESCAPE) {
                            close_sheet(a);
                        } else if is(VK_RETURN) {
                            start_download(a);
                        }
                        invalidate(hwnd);
                        return Action::None;
                    }
                    if a.ui.rename.is_some() {
                        if is(VK_ESCAPE) {
                            a.ui.rename = None;
                        } else if is(VK_RETURN) {
                            save_rename(a);
                        }
                        invalidate(hwnd);
                        return Action::None;
                    }
                    if a.ui.search_focus {
                        if is(VK_ESCAPE) {
                            if a.ui.search.as_deref().is_some_and(|q| !q.is_empty()) {
                                a.ui.search = Some(String::new());
                            } else {
                                close_search(a);
                            }
                        } else if is(VK_RETURN) {
                            a.ui.search_focus = false;
                        }
                        invalidate(hwnd);
                        return Action::None;
                    }
                    match key {
                        _ if is(VK_ESCAPE) && a.ui.view == ui::View::Stats => a.ui.view = ui::View::List,
                        _ if is(VK_ESCAPE) => {
                            if a.ui.selected.is_empty() && a.ui.search.is_some() {
                                close_search(a);
                            }
                            a.ui.selected.clear();
                        }
                        _ if is(VK_DELETE) => return Action::ConfirmRemove,
                        _ if is(VK_SPACE) => toggle_selected(a),
                        _ if is(VK_F2) => {
                            if a.ui.selected.len() == 1 {
                                let h = a.ui.selected.iter().next().cloned().unwrap_or_default();
                                start_rename(a, &h);
                            }
                        }
                        k if k == b'F' as u16 && ctrl => open_search(a),
                        k if k == b'A' as u16 && ctrl => {
                            a.ui.selected = a.ui.visible_hashes().into_iter().collect();
                        }
                        k if k == b'O' as u16 && ctrl => return Action::OpenFiles,
                        _ => {}
                    }
                    invalidate(hwnd);
                    Action::None
                });
                let typing =
                    with(|a| a.ui.sheet.is_some() || a.ui.rename.is_some() || a.ui.search_focus).unwrap_or(true);
                if key == b'V' as u16 && ctrl && !typing {
                    paste(hwnd);
                } else if let Some(action) = action {
                    run_action(hwnd, action);
                }
                LRESULT(0)
            }
            WM_CHAR => {
                let c = wp.0 as u32;
                with(|a| {
                    if c >= 0x20
                        && c != 0x7F
                        && let Some(ch) = char::from_u32(c)
                        && let Some(text) = a.ui.focused_text()
                        && text.chars().count() < 120
                    {
                        text.push(ch);
                        a.ui.caret_on = true;
                        invalidate(hwnd);
                    }
                });
                LRESULT(0)
            }
            WM_DROPFILES => {
                let drop = HDROP(wp.0 as *mut _);
                let n = DragQueryFileW(drop, u32::MAX, None);
                let mut files = Vec::new();
                for i in 0..n {
                    let mut buf = vec![0u16; 1024];
                    let len = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                    files.push(String::from_utf16_lossy(&buf[..len]));
                }
                DragFinish(drop);
                let _ = SetForegroundWindow(hwnd);
                with(|a| {
                    let torrents: Vec<String> =
                        files.into_iter().filter(|f| f.to_ascii_lowercase().ends_with(".torrent")).collect();
                    if torrents.is_empty() {
                        notify(
                            a,
                            tt("Перетащите файл с расширением .torrent", "Drop a file with the .torrent extension"),
                        );
                    } else {
                        add_sources(a, torrents);
                    }
                });
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if (lp.0 & 0xFFFF) as u32 == HTCLIENT
                    && with(|a| a.ui.hover.is_some_and(Hit::is_text_field)).unwrap_or(false)
                    && let Ok(c) = LoadCursorW(None, IDC_IBEAM)
                {
                    SetCursor(Some(c));
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_NCCALCSIZE if wp.0 != 0 => {
                // No caption: keep the default side and bottom borders (they
                // are invisible resize handles), take the top for the client.
                let params = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
                let top = params.rgrc[0].top;
                let r = DefWindowProcW(hwnd, msg, wp, lp);
                params.rgrc[0].top = top;
                if IsZoomed(hwnd).as_bool() {
                    // A maximized window hangs its frame off-screen.
                    params.rgrc[0].top += frame_thickness(hwnd);
                }
                r
            }
            WM_NCHITTEST => {
                let r = DefWindowProcW(hwnd, msg, wp, lp);
                if r.0 as u32 != HTCLIENT {
                    return r;
                }
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                let _ = ScreenToClient(hwnd, &mut pt);
                let zoomed = IsZoomed(hwnd).as_bool();
                if !zoomed && pt.y < frame_thickness(hwnd) {
                    let mut rc = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rc);
                    let corner = 16;
                    return LRESULT(if pt.x < corner {
                        HTTOPLEFT
                    } else if pt.x > rc.right - corner {
                        HTTOPRIGHT
                    } else {
                        HTTOP
                    } as isize);
                }
                let s = scale_of(hwnd);
                let drag = with(|a| a.ui.is_drag_area(pt.x as f32 / s, pt.y as f32 / s)).unwrap_or(false);
                LRESULT(if drag { HTCAPTION } else { HTCLIENT } as isize)
            }
            WM_SIZE => {
                invalidate(hwnd);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_GETMINMAXINFO => {
                let s = scale_of(hwnd);
                let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                mmi.ptMinTrackSize = POINT { x: (480.0 * s) as i32, y: (600.0 * s) as i32 };
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                LRESULT(0)
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED => {
                with(|a| {
                    a.ui.theme = Theme::current();
                    apply_frame(hwnd, &a.ui.theme);
                    invalidate(hwnd);
                });
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                tray::remove(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
