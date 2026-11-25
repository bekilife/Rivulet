//! What the window shows: title bar, toolbar, download cards, statistics,
//! and the "new download" and rename sheets. Immediate mode: every paint
//! lays everything out again and records the clickable rectangles in `hits`
//! for the next mouse event.

use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, Instant},
};

use rivulet::engine::{Draft, Snapshot, Status};

use crate::{
    fmt,
    gfx::{Align, Color, Gfx, Rect},
    icons,
    lang::tt,
    stats::{Period, Stats},
    theme::Theme,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Add,
    Toggle,
    Remove,
    Settings,
    Card(usize),
    Expand(usize),
    SheetFile(usize),
    SheetAll,
    SheetFolder,
    SheetAlias,
    SheetCancel,
    SheetOk,
    SheetFiles,
    SheetBody,
    Search,
    SearchField,
    SearchClear,
    RenameField,
    RenameCancel,
    RenameOk,
    RenameBody,
    WinMin,
    WinMax,
    WinClose,
    Stats,
    Summary,
    Back,
    Period(usize),
    Bar(usize),
    StatsReset,
}

impl Hit {
    pub fn clickable(self) -> bool {
        !matches!(self, Hit::SheetBody | Hit::SheetFiles | Hit::RenameBody | Hit::Bar(_))
    }
    pub fn is_text_field(self) -> bool {
        matches!(self, Hit::SheetAlias | Hit::SearchField | Hit::RenameField)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    List,
    Stats,
}

pub const PERIODS: [(Period, &str, &str); 3] =
    [(Period::Week, "7 дней", "7 days"), (Period::Month, "30 дней", "30 days"), (Period::All, "Всё время", "All time")];

/// The "rename download" dialog.
pub struct Rename {
    pub hash: String,
    /// Name inside the torrent, used when the field is left empty.
    pub original: String,
    pub text: String,
}

pub enum SheetState {
    Loading,
    Ready(Draft),
    Failed(String),
}

pub struct Sheet {
    pub source: String,
    pub state: SheetState,
    pub token: u64,
    pub alias: String,
    pub alias_focus: bool,
    pub checked: Vec<bool>,
    pub folder: PathBuf,
    pub free: Option<u64>,
    pub file_scroll: f32,
    /// (this one, total) when several torrents were opened together.
    pub position: (usize, usize),
    pub busy: bool,
}

impl Sheet {
    pub fn selected_bytes(&self) -> u64 {
        match &self.state {
            SheetState::Ready(d) => d.files.iter().zip(&self.checked).filter(|(_, c)| **c).map(|(f, _)| f.size).sum(),
            _ => 0,
        }
    }
    pub fn selected_count(&self) -> usize {
        self.checked.iter().filter(|c| **c).count()
    }
    pub fn can_start(&self) -> bool {
        matches!(self.state, SheetState::Ready(_)) && self.selected_count() > 0 && !self.busy
    }
}

pub struct Ui {
    pub theme: Theme,
    pub snaps: Vec<Snapshot>,
    pub selected: HashSet<String>,
    pub expanded: HashSet<String>,
    pub scroll: f32,
    pub hover: Option<Hit>,
    pub pressed: Option<Hit>,
    pub sheet: Option<Sheet>,
    pub precise: bool,
    pub notice: Option<(String, Instant)>,
    pub hits: Vec<(Rect, Hit)>,
    pub spin: f32,
    pub caret_on: bool,
    /// Search text while the search field is open.
    pub search: Option<String>,
    pub search_focus: bool,
    pub rename: Option<Rename>,
    pub view: View,
    /// Index into PERIODS.
    pub period: usize,
    pub maximized: bool,
    content_h: f32,
    view_h: f32,
    files_view: Rect,
    files_content_h: f32,
}

const PAD: f32 = 16.0;
/// Top of the toolbar row, below the title row.
const BAR_Y: f32 = 52.0;
/// Top of the content below the toolbar.
const CONTENT_Y: f32 = 108.0;
const SUMMARY_H: f32 = 64.0;
const GAP: f32 = 10.0;
/// Height of the title row the window is dragged by.
pub const TITLE_H: f32 = 44.0;

impl Ui {
    pub fn new(theme: Theme, precise: bool) -> Ui {
        Ui {
            theme,
            snaps: Vec::new(),
            selected: HashSet::new(),
            expanded: HashSet::new(),
            scroll: 0.0,
            hover: None,
            pressed: None,
            sheet: None,
            precise,
            notice: None,
            hits: Vec::new(),
            spin: 0.0,
            caret_on: true,
            search: None,
            search_focus: false,
            rename: None,
            view: View::List,
            period: 1,
            maximized: false,
            content_h: 0.0,
            view_h: 0.0,
            files_view: Rect::default(),
            files_content_h: 0.0,
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }

    /// Whether (x, y) is empty space the window can be dragged by.
    pub fn is_drag_area(&self, x: f32, y: f32) -> bool {
        let row = if self.sheet.is_some() || self.rename.is_some() { TITLE_H } else { CONTENT_Y - 6.0 };
        y < row && self.hit(x, y).is_none()
    }

    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    /// Scrolls the download list, or the file list in the sheet when the
    /// pointer is over it.
    pub fn wheel(&mut self, x: f32, y: f32, delta: f32) {
        if let Some(sheet) = &mut self.sheet {
            if self.files_view.contains(x, y) {
                let max = (self.files_content_h - self.files_view.h).max(0.0);
                sheet.file_scroll = (sheet.file_scroll - delta).clamp(0.0, max);
            }
            return;
        }
        if self.view == View::List {
            let max = (self.content_h - self.view_h).max(0.0);
            self.scroll = (self.scroll - delta).clamp(0.0, max);
        }
    }

    /// Whether a download passes the search (by its own name or alias).
    pub fn matches(&self, s: &Snapshot) -> bool {
        let Some(q) = self.search.as_deref().map(str::trim).filter(|q| !q.is_empty()) else { return true };
        let q = q.to_lowercase();
        s.name.to_lowercase().contains(&q) || s.alias.as_deref().is_some_and(|a| a.to_lowercase().contains(&q))
    }

    /// Hashes of the downloads currently shown.
    pub fn visible_hashes(&self) -> Vec<String> {
        self.snaps.iter().filter(|s| self.matches(s)).map(|s| s.info_hash.clone()).collect()
    }

    /// The text field that has keyboard focus, if any.
    pub fn focused_text(&mut self) -> Option<&mut String> {
        if let Some(sheet) = self.sheet.as_mut() {
            return sheet.alias_focus.then_some(&mut sheet.alias);
        }
        if let Some(r) = self.rename.as_mut() {
            return Some(&mut r.text);
        }
        if self.search_focus {
            return self.search.as_mut();
        }
        None
    }

    pub fn selected_snaps(&self) -> Vec<&Snapshot> {
        self.snaps.iter().filter(|s| self.selected.contains(&s.info_hash)).collect()
    }

    /// True when the toggle button would pause (something selected is running).
    pub fn toggle_pauses(&self) -> bool {
        self.selected_snaps()
            .iter()
            .any(|s| matches!(s.status, Status::Downloading | Status::Seeding | Status::Checking))
    }

    pub fn notice_active(&self) -> bool {
        self.notice.as_ref().is_some_and(|(_, t)| t.elapsed() < Duration::from_secs(4))
    }

    // ---------------------------------------------------------------- paint

    pub fn paint(&mut self, g: &Gfx, w: f32, h: f32, stats: &Stats) {
        self.hits.clear();
        let t = self.theme;
        g.fill_rect(Rect::new(0.0, 0.0, w, h), t.backdrop);

        match self.view {
            View::List => {
                self.paint_list(g, w, h, stats);
                self.paint_toolbar(g, w);
                self.paint_summary(g, w, h, stats);
            }
            View::Stats => self.paint_stats(g, w, h, stats),
        }
        self.paint_title(g, w);
        if self.notice_active() {
            self.paint_notice(g, w, h);
        }
        if self.sheet.is_some() || self.rename.is_some() {
            // A sheet takes all input while it is open; the window buttons stay.
            self.hits.retain(|(_, h)| matches!(h, Hit::WinMin | Hit::WinMax | Hit::WinClose));
            if self.sheet.is_some() {
                self.paint_sheet(g, w, h);
            } else {
                self.paint_rename(g, w, h);
            }
        }
    }

    fn button_bg(&self, g: &Gfx, hit: Hit, r: Rect, rad: f32) {
        if self.pressed == Some(hit) {
            g.fill_rrect(r, rad, self.theme.press);
        } else if self.hover == Some(hit) {
            g.fill_rrect(r, rad, self.theme.hover);
        }
    }

    /// App name on the left, our own minimize / maximize / close on the right.
    fn paint_title(&mut self, g: &Gfx, w: f32) {
        let t = self.theme;
        g.text("Rivulet", 15.0).weight(650).color(t.text).draw(PAD, 8.0 + (32.0 - g.normal_lh(15.0)) / 2.0);
        let close = Rect::new(w - 8.0 - 32.0, 8.0, 32.0, 32.0);
        let max = Rect::new(close.x - 34.0, 8.0, 32.0, 32.0);
        let min = Rect::new(max.x - 34.0, 8.0, 32.0, 32.0);
        for (r, hit) in [(min, Hit::WinMin), (max, Hit::WinMax), (close, Hit::WinClose)] {
            let hot = self.hover == Some(hit) || self.pressed == Some(hit);
            let mut c = t.text2;
            if hot && hit == Hit::WinClose {
                g.fill_rrect(r, 9.0, t.close_hover);
                c = Color::white(1.0);
            } else {
                self.button_bg(g, hit, r, 9.0);
            }
            let (cx, cy) = (r.x + 16.0, r.y + 16.0);
            match hit {
                Hit::WinMin => g.line(cx - 4.5, cy, cx + 4.5, cy, 1.3, c, true),
                Hit::WinMax if self.maximized => {
                    g.stroke_rrect(Rect::new(cx - 4.5, cy - 2.5, 7.0, 7.0), 1.8, 1.2, c);
                    g.line(cx - 2.0, cy - 4.5, cx + 2.5, cy - 4.5, 1.2, c, true);
                    g.line(cx + 4.5, cy - 2.5, cx + 4.5, cy + 2.0, 1.2, c, true);
                    g.line(cx + 2.5, cy - 4.5, cx + 4.5, cy - 2.5, 1.2, c, true);
                }
                Hit::WinMax => g.stroke_rrect(Rect::new(cx - 4.5, cy - 4.5, 9.0, 9.0), 2.2, 1.3, c),
                _ => {
                    g.line(cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, 1.3, c, true);
                    g.line(cx + 4.0, cy - 4.0, cx - 4.0, cy + 4.0, 1.3, c, true);
                }
            }
            self.hits.push((r, hit));
        }
    }

    /// A rounded group of 36 px icon buttons. `items`: hit, icon, enabled, highlighted.
    fn button_group(&mut self, g: &Gfx, x: f32, items: &[(Hit, &'static str, bool, bool)]) -> Rect {
        let t = self.theme;
        let r = Rect::new(x, BAR_Y, 6.0 + items.len() as f32 * 38.0 - 2.0, 44.0);
        g.fill_rrect(r, 13.0, t.card);
        g.stroke_rrect(r.inset(0.5), 12.5, 1.0, t.card_border);
        for (i, &(hit, icon, enabled, lit)) in items.iter().enumerate() {
            let b = Rect::new(r.x + 3.0 + i as f32 * 38.0, r.y + 4.0, 36.0, 36.0);
            if lit {
                g.fill_rrect(b, 10.0, t.accent_soft);
            }
            if enabled {
                self.button_bg(g, hit, b, 10.0);
                self.hits.push((b, hit));
            }
            let c = if lit {
                t.accent
            } else if enabled {
                t.text
            } else {
                t.text3
            };
            icons::draw(g, icon, b.x + 8.0, b.y + 8.0, 20.0, 1.8, c);
        }
        r
    }

    fn paint_toolbar(&mut self, g: &Gfx, w: f32) {
        let t = self.theme;
        let any = !self.selected.is_empty();
        let toggle_icon = if self.toggle_pauses() { icons::PAUSE } else { icons::PLAY };
        let left = self.button_group(
            g,
            PAD,
            &[
                (Hit::Add, icons::PLUS, true, true),
                (Hit::Toggle, toggle_icon, any, false),
                (Hit::Remove, icons::TRASH, any, false),
            ],
        );
        let right_w = 6.0 + 3.0 * 38.0 - 2.0;
        let searching = self.search.is_some();
        let right = self.button_group(
            g,
            w - PAD - right_w,
            &[
                (Hit::Stats, icons::CHART, true, false),
                (Hit::Search, icons::SEARCH, true, searching),
                (Hit::Settings, icons::SLIDERS, true, false),
            ],
        );
        if let Some(q) = &self.search {
            let f = Rect::new(left.x + left.w + 10.0, BAR_Y, right.x - 10.0 - (left.x + left.w + 10.0), 44.0);
            g.fill_rrect(f, 13.0, t.card);
            if self.search_focus {
                g.stroke_rrect(f.inset(0.75), 12.25, 1.5, t.accent);
            } else {
                g.stroke_rrect(f.inset(0.5), 12.5, 1.0, t.card_border);
            }
            let field = Rect::new(f.x, f.y, f.w - 40.0, f.h);
            self.text_field(g, field, q, tt("Поиск по названию", "Search by name"), self.search_focus, 14.0);
            self.hits.push((f, Hit::SearchField));
            let c = Rect::new(f.x + f.w - 40.0, f.y + 6.0, 32.0, 32.0);
            self.button_bg(g, Hit::SearchClear, c, 9.0);
            icons::draw(g, icons::CLOSE, c.x + 7.0, c.y + 7.0, 18.0, 1.7, t.text2);
            self.hits.push((c, Hit::SearchClear));
        }
    }

    /// "За 30 дней ↓ … ↑ …" card at the bottom of the list; opens statistics.
    fn paint_summary(&mut self, g: &Gfx, w: f32, h: f32, stats: &Stats) {
        let t = self.theme;
        let r = Rect::new(PAD, h - PAD - SUMMARY_H, w - 2.0 * PAD, SUMMARY_H);
        g.fill_rrect(r, 16.0, t.card);
        let hot = self.hover == Some(Hit::Summary);
        g.stroke_rrect(r.inset(0.5), 15.5, 1.0, if hot { t.card_hover_border } else { t.card_border });
        self.hits.push((r, Hit::Summary));

        // Last 7 days, a downloaded and an uploaded bar per day.
        let days = stats.recent(7);
        let max = days.iter().map(|(d, u)| (*d).max(*u)).max().unwrap_or(0).max(1);
        let mut x = r.x + 16.0;
        let base = r.y + r.h / 2.0 + 14.0;
        for (d, u) in &days {
            for (v, c) in [(*d, t.accent), (*u, t.green)] {
                let bh = if v == 0 { 3.0 } else { (v as f32 / max as f32 * 28.0).max(4.0) };
                g.fill_rrect(Rect::new(x, base - bh, 4.0, bh), 2.0, if v == 0 { t.track } else { c });
                x += 6.0;
            }
            x += 2.0;
        }
        let month = stats.period(Period::Month);
        let tx = x + 10.0;
        g.text(tt("За 30 дней", "Last 30 days"), 12.0).color(t.text3).draw(tx, r.y + 12.0);
        let down = format!("↓ {}", fmt::size(month.down, false));
        let up = format!("↑ {}", fmt::size(month.up, false));
        let dw = g.text(&down, 15.0).weight(600).color(t.accent).draw(tx, r.y + 30.0);
        g.text(&up, 15.0).weight(600).color(t.green).draw(tx + dw + 12.0, r.y + 30.0);
        icons::draw(g, icons::CHEVRON_RIGHT, r.x + r.w - 32.0, r.y + 23.0, 18.0, 1.8, t.text2);
        let label = tt("Статистика", "Statistics");
        let lw = g.text(label, 13.0).weight(600).measure();
        g.text(label, 13.0)
            .weight(600)
            .color(t.text2)
            .draw(r.x + r.w - 38.0 - lw, r.y + (r.h - g.normal_lh(13.0)) / 2.0);
    }

    fn card_height(&self, s: &Snapshot) -> f32 {
        let expanded = self.expanded.contains(&s.info_hash);
        if is_compact(s) && !expanded {
            return 64.0;
        }
        let mut h = 16.0 + 22.0 + 8.0 + 6.0 + 12.0 + 30.0 + 16.0;
        if s.alias.is_some() || s.error.is_some() {
            h += 24.0;
        }
        if expanded {
            h += 14.0 + 18.0 + 6.0 + 32.0 + 4.0;
        }
        h
    }

    fn paint_list(&mut self, g: &Gfx, w: f32, h: f32, stats: &Stats) {
        let t = self.theme;
        let top = CONTENT_Y;
        let bottom = h - PAD - SUMMARY_H - GAP;
        self.view_h = (bottom - top).max(0.0);
        if self.snaps.is_empty() {
            self.content_h = 0.0;
            self.scroll = 0.0;
            let cy = top + (bottom - top) / 2.0 - 50.0;
            g.text("Rivulet", 44.0).weight(700).color(t.accent).width(w).align(Align::Center).draw(0.0, cy);
            g.text(
                tt(
                    "Перетащите сюда .torrent-файл, нажмите «+» или вставьте magnet-ссылку (Ctrl+V)",
                    "Drop a .torrent file here, press + or paste a magnet link (Ctrl+V)",
                ),
                14.0,
            )
            .color(t.text2)
            .width(320.0)
            .align(Align::Center)
            .wrap()
            .draw((w - 320.0) / 2.0, cy + 64.0);
            return;
        }
        let shown: Vec<usize> = (0..self.snaps.len()).filter(|&i| self.matches(&self.snaps[i])).collect();
        if shown.is_empty() {
            self.content_h = 0.0;
            self.scroll = 0.0;
            let q = self.search.clone().unwrap_or_default();
            g.text(&tr!("Ничего не найдено по запросу «{}»", "Nothing found for “{}”", q.trim()), 14.0)
                .color(t.text2)
                .width(w - 64.0)
                .align(Align::Center)
                .ellipsis()
                .draw(32.0, top + 40.0);
            return;
        }
        let heights: Vec<f32> = shown.iter().map(|&i| self.card_height(&self.snaps[i])).collect();
        self.content_h = heights.iter().sum::<f32>() + GAP * heights.len() as f32;
        let max = (self.content_h - self.view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);

        g.push_clip_rect(Rect::new(0.0, top, w, bottom - top));
        let mut y = top - self.scroll;
        for (k, &i) in shown.iter().enumerate() {
            let ch = heights[k];
            if y + ch >= top && y <= bottom {
                let r = Rect::new(PAD, y, w - 2.0 * PAD, ch);
                self.paint_card(g, i, r, stats);
            }
            y += ch + GAP;
        }
        g.pop_clip_rect();
        // Keep hits inside the visible list area.
        self.hits.retain(|(r, hit)| !matches!(hit, Hit::Card(_) | Hit::Expand(_)) || (r.y + r.h > top && r.y < bottom));

        if max > 0.0 {
            let bar_h = (self.view_h * self.view_h / self.content_h).max(30.0);
            let bar_y = top + (self.view_h - bar_h) * (self.scroll / max);
            g.fill_rrect(Rect::new(w - 6.0, bar_y + 2.0, 3.0, bar_h - 4.0), 1.5, t.text3.alpha(0.6));
        }
    }

    fn paint_card(&mut self, g: &Gfx, i: usize, r: Rect, stats: &Stats) {
        let t = self.theme;
        let s = &self.snaps[i];
        let selected = self.selected.contains(&s.info_hash);
        let expanded = self.expanded.contains(&s.info_hash);
        let hovered = matches!(self.hover, Some(Hit::Card(j) | Hit::Expand(j)) if j == i);
        g.fill_rrect(r, 16.0, t.card);
        if selected {
            g.stroke_rrect(r.inset(0.75), 15.25, 1.5, t.text);
        } else {
            g.stroke_rrect(r.inset(0.5), 15.5, 1.0, if hovered { t.card_hover_border } else { t.card_border });
        }
        self.hits.push((r, Hit::Card(i)));

        let title = s.alias.clone().unwrap_or_else(|| s.name.clone());
        let precise = self.precise;
        let (status_color, status_icon) = status_look(&t, s);
        let compact = is_compact(s) && !expanded;

        // Expand button and selection mark on the right.
        let eb = if compact {
            Rect::new(r.x + r.w - 12.0 - 30.0, r.y + 17.0, 30.0, 30.0)
        } else {
            Rect::new(r.x + r.w - 12.0 - 30.0, r.y + 10.0, 30.0, 30.0)
        };
        self.button_bg(g, Hit::Expand(i), eb, 8.0);
        icons::draw(
            g,
            if expanded { icons::CHEVRON_UP } else { icons::CHEVRON_DOWN },
            eb.x + 5.0,
            eb.y + 5.0,
            20.0,
            1.7,
            t.text2,
        );
        self.hits.push((eb, Hit::Expand(i)));
        let mut right = eb.x - 6.0;
        if selected {
            let cx = right - 10.0;
            g.fill_circle(cx, eb.y + 15.0, 10.0, t.text);
            icons::draw(g, icons::CHECK, cx - 7.0, eb.y + 8.0, 14.0, 2.0, t.backdrop);
            right = cx - 18.0;
        }

        if compact {
            let pill = Rect::new(r.x + 14.0, r.y + 14.0, 36.0, 36.0);
            g.fill_rrect(pill, 11.0, status_color.alpha(0.15));
            icons::draw(g, status_icon, pill.x + 8.0, pill.y + 8.0, 20.0, 1.9, status_color);
            let label = status_label(s);
            let lw = g.text(label, 13.0).measure();
            g.text(label, 13.0).color(t.text2).draw(right - lw, r.y + (64.0 - g.normal_lh(13.0)) / 2.0);
            let tx = pill.x + 48.0;
            let tw = right - lw - 12.0 - tx;
            let uploaded = stats.uploaded(&s.info_hash).unwrap_or(s.uploaded);
            let note = if uploaded > 0 {
                tr!("{} · роздано {}", "{} · shared {}", fmt::size(s.size, precise), fmt::size(uploaded, precise))
            } else {
                fmt::size(s.size, precise)
            };
            g.text(&title, 15.0).weight(600).color(t.text).width(tw).ellipsis().draw(tx, r.y + 12.0);
            g.text(&note, 12.0).color(t.text3).width(tw).ellipsis().draw(tx, r.y + 34.0);
            return;
        }

        let x = r.x + 18.0;
        let inner_w = r.w - 36.0;
        // Name, then "2,9 из 4,7 ГБ" next to the expand button.
        let amount = if s.status == Status::Downloading || s.status == Status::Paused || s.status == Status::Checking {
            tr!("{} из {}", "{} of {}", fmt::size(s.downloaded, precise), fmt::size(s.size, precise))
        } else {
            fmt::size(s.size, precise)
        };
        let aw = g.text(&amount, 12.0).measure();
        g.text(&amount, 12.0).color(t.text3).draw(right - aw, r.y + 18.0);
        g.text(&title, 15.0).weight(600).color(t.text).width(right - aw - 12.0 - x).ellipsis().draw(x, r.y + 14.0);

        // Progress bar.
        let bar = Rect::new(x, r.y + 46.0, inner_w, 6.0);
        g.fill_rrect(bar, 3.0, t.track);
        let fill_w = (bar.w * s.progress as f32).clamp(0.0, bar.w);
        if fill_w > 0.5 {
            g.fill_rrect(Rect::new(bar.x, bar.y, fill_w.max(6.0), bar.h), 3.0, status_color);
        }

        // Status row: [icon 63%] Загрузка ........ 6,2 МБ/с | 2 мин
        let row_y = r.y + 64.0;
        let cy = row_y + (30.0 - g.normal_lh(14.0)) / 2.0;
        let pct = fmt::percent(s.progress, precise);
        let pw = g.text(&pct, 14.0).weight(600).measure();
        let pill = Rect::new(x, row_y, 9.0 + 16.0 + 6.0 + pw + 11.0, 30.0);
        g.fill_rrect(pill, 9.0, status_color.alpha(0.15));
        icons::draw(g, status_icon, pill.x + 9.0, pill.y + 7.0, 16.0, 2.0, status_color);
        g.text(&pct, 14.0).weight(600).color(status_color).draw(pill.x + 31.0, cy);
        g.text(status_label(s), 14.0).weight(500).color(t.text).draw(pill.x + pill.w + 10.0, cy);

        if matches!(s.status, Status::Downloading | Status::Seeding) {
            let speed_bps = if s.status == Status::Seeding { s.up_bps } else { s.down_bps };
            let sp = fmt::speed(speed_bps, precise);
            let mut rx = x + inner_w;
            if let Some(e) = s.eta_secs.map(fmt::eta) {
                let ew = g.text(&e, 14.0).measure();
                g.text(&e, 14.0).color(t.text2).draw(rx - ew, cy);
                rx -= ew + 10.0;
                g.fill_rect(Rect::new(rx - 1.0, row_y + 7.0, 1.0, 16.0), t.separator);
                rx -= 11.0;
            }
            let sw = g.text(&sp, 14.0).weight(600).measure();
            g.text(&sp, 14.0).weight(600).color(speed_color(&t, speed_bps)).draw(rx - sw, cy);
        }

        let mut y = row_y + 30.0 + 10.0;
        if let Some(err) = &s.error {
            g.text(err, 13.0).color(t.red).width(inner_w).ellipsis().draw(x, y);
            y += 24.0;
        } else if s.alias.is_some() {
            let sub = tr!("В торренте — {}", "In the torrent: {}", s.name);
            g.text(&sub, 13.0).color(t.text2).width(inner_w).ellipsis().draw(x, y);
            y += 24.0;
        }

        if expanded {
            g.fill_rect(Rect::new(x, y + 4.0, inner_w, 1.0), t.separator);
            let ly = y + 16.0;
            let vy = ly + 22.0;
            let uploaded = stats.uploaded(&s.info_hash).unwrap_or(s.uploaded);
            let cols: [(&str, &'static str, String); 4] = [
                (tt("Сиды и пиры", "Seeds & peers"), icons::PEERS, format!("{} · {}", s.seeds, s.peers)),
                (tt("Отдача", "Upload"), icons::UP, fmt::speed(s.up_bps, precise)),
                (tt("Отдано", "Uploaded"), icons::ROTATE, fmt::size(uploaded, precise)),
                (tt("Размер", "Size"), icons::BOX, fmt::size(s.size, precise)),
            ];
            let col_w = inner_w / 4.0;
            for (k, (label, icon, value)) in cols.iter().enumerate() {
                let cx = x + k as f32 * col_w;
                g.text(label, 12.0).color(t.text2).width(col_w - 6.0).ellipsis().draw(cx, ly);
                let vw = g.text(value, 13.0).weight(600).measure();
                let chip = Rect::new(cx, vy, (8.0 + 16.0 + 6.0 + vw + 10.0).min(col_w - 6.0), 30.0);
                g.fill_rrect(chip, 8.0, t.chip);
                icons::draw(g, icon, chip.x + 8.0, chip.y + 7.0, 16.0, 1.6, t.text2);
                g.text(value, 13.0)
                    .weight(600)
                    .color(t.text)
                    .width(chip.w - 34.0)
                    .ellipsis()
                    .draw(chip.x + 30.0, vy + (30.0 - g.normal_lh(13.0)) / 2.0);
            }
        }
    }

    // ---------------------------------------------------------------- statistics

    fn paint_stats(&mut self, g: &Gfx, w: f32, h: f32, stats: &Stats) {
        let t = self.theme;
        let precise = self.precise;
        let pv = stats.period(PERIODS[self.period].0);

        // Back, title, period switch.
        let back = Rect::new(PAD, BAR_Y + 2.0, 40.0, 40.0);
        g.fill_rrect(back, 12.0, t.card);
        g.stroke_rrect(back.inset(0.5), 11.5, 1.0, t.card_border);
        self.button_bg(g, Hit::Back, back.inset(1.0), 11.0);
        icons::draw(g, icons::BACK, back.x + 10.0, back.y + 10.0, 20.0, 1.8, t.text);
        self.hits.push((back, Hit::Back));
        g.text(tt("Статистика", "Statistics"), 20.0)
            .weight(650)
            .color(t.text)
            .draw(back.x + back.w + 12.0, BAR_Y + (44.0 - g.normal_lh(20.0)) / 2.0);
        let widths: Vec<f32> =
            PERIODS.iter().map(|(_, ru, en)| g.text(tt(ru, en), 13.0).weight(600).measure() + 28.0).collect();
        let seg_w = widths.iter().sum::<f32>() + 6.0 + 2.0 * (widths.len() - 1) as f32;
        let seg = Rect::new(w - PAD - seg_w, BAR_Y + 3.0, seg_w, 38.0);
        g.fill_rrect(seg, 12.0, t.card);
        g.stroke_rrect(seg.inset(0.5), 11.5, 1.0, t.card_border);
        let mut sx = seg.x + 3.0;
        for (k, (_, ru, en)) in PERIODS.iter().enumerate() {
            let label = tt(ru, en);
            let b = Rect::new(sx, seg.y + 3.0, widths[k], 32.0);
            let on = k == self.period;
            if on {
                g.fill_rrect(b, 9.0, t.seg_on);
            } else {
                self.button_bg(g, Hit::Period(k), b, 9.0);
            }
            g.text(label, 13.0)
                .weight(600)
                .color(if on { t.seg_on_text } else { t.text2 })
                .width(b.w)
                .align(Align::Center)
                .draw(b.x, b.y + (32.0 - g.normal_lh(13.0)) / 2.0);
            self.hits.push((b, Hit::Period(k)));
            sx += widths[k] + 2.0;
        }

        g.push_clip_rect(Rect::new(0.0, CONTENT_Y, w, h - CONTENT_Y));

        // Totals card: downloaded | uploaded | ratio ring.
        let hero = Rect::new(PAD, CONTENT_Y, w - 2.0 * PAD, 142.0);
        g.fill_rrect(hero, 16.0, t.card);
        g.stroke_rrect(hero.inset(0.5), 15.5, 1.0, t.card_border);
        g.text(&pv.range, 12.0).color(t.text3).width(hero.w - 36.0).ellipsis().draw(hero.x + 18.0, hero.y + 12.0);
        let inner = hero.w - 36.0;
        let col_w = (inner - 68.0 - 4.0 * 18.0 - 2.0) / 2.0;
        let cy = hero.y + 40.0;
        let cols = [
            (
                tt("Скачано", "Downloaded"),
                t.accent,
                pv.down,
                fmt::count(pv.done as usize, ["загрузка", "загрузки", "загрузок"], ["download", "downloads"]),
            ),
            (
                tt("Роздано", "Shared"),
                t.green,
                pv.up,
                fmt::count(pv.seeded, ["раздача", "раздачи", "раздач"], ["seeded torrent", "seeded torrents"]),
            ),
        ];
        let mut cx = hero.x + 18.0;
        for (label, color, bytes, note) in &cols {
            g.fill_circle(cx + 4.0, cy + 9.0, 4.0, *color);
            g.text(label, 13.0).color(t.text2).draw(cx + 14.0, cy);
            g.text(&fmt::size(*bytes, precise), 30.0)
                .weight(700)
                .color(t.text)
                .width(col_w)
                .ellipsis()
                .draw(cx, cy + 20.0);
            g.text(note, 12.0).color(t.text3).width(col_w).ellipsis().draw(cx, cy + 64.0);
            cx += col_w + 18.0;
            g.fill_rect(Rect::new(cx, cy, 1.0, 80.0), t.separator);
            cx += 19.0;
        }
        let ratio = if pv.down > 0 { pv.up as f64 / pv.down as f64 } else { 0.0 };
        let (rx, ry) = (hero.x + hero.w - 18.0 - 34.0, cy + 34.0);
        g.ring(rx, ry, 28.0, 7.0, (ratio.min(1.0) * 100.0) as f32, t.green, t.track);
        let rt = fmt::ratio(ratio);
        g.text(&rt, 16.0)
            .weight(700)
            .color(t.text)
            .width(68.0)
            .align(Align::Center)
            .draw(rx - 34.0, ry - g.normal_lh(16.0) / 2.0);
        g.text(tt("Рейтинг", "Ratio"), 12.0).color(t.text2).width(68.0).align(Align::Center).draw(rx - 34.0, ry + 38.0);

        // Chart card.
        let chart = Rect::new(PAD, hero.y + hero.h + 12.0, w - 2.0 * PAD, 244.0);
        g.fill_rrect(chart, 16.0, t.card);
        g.stroke_rrect(chart.inset(0.5), 15.5, 1.0, t.card_border);
        g.text(pv.title, 15.0).weight(600).color(t.text).draw(chart.x + 18.0, chart.y + 14.0);
        let mut lx = chart.x + chart.w - 18.0;
        for (label, color) in [(tt("Роздано", "Shared"), t.green), (tt("Скачано", "Downloaded"), t.accent)]
        {
            let lw = g.text(label, 12.0).measure();
            lx -= lw;
            g.text(label, 12.0).color(t.text2).draw(lx, chart.y + 17.0);
            g.fill_circle(lx - 10.0, chart.y + 25.0, 4.0, color);
            lx -= 28.0;
        }
        let ax = chart.x + 18.0;
        let ay = chart.y + 50.0;
        let aw = chart.w - 36.0 - 50.0;
        let ah = 150.0;
        let max = pv.bars.iter().map(|(d, u, _)| (*d).max(*u)).max().unwrap_or(0);
        let (y_max, y_labels) = axis_scale(max);
        g.fill_rect(Rect::new(ax, ay, aw, 1.0), t.grid);
        g.fill_rect(Rect::new(ax, ay + ah / 2.0, aw, 1.0), t.grid);
        g.fill_rect(Rect::new(ax, ay + ah, aw, 1.0), t.card_border);
        for (k, label) in y_labels.iter().enumerate() {
            let yy = ay + ah * k as f32 / 2.0 - g.normal_lh(11.0) / 2.0;
            g.text(label, 11.0).color(t.text3).width(42.0).align(Align::Right).draw(ax + aw + 8.0, yy);
        }
        let n = pv.bars.len().max(1);
        let slot = aw / n as f32;
        let pair_gap = if n > 14 { 3.0 } else { 14.0 };
        let bw = ((slot - pair_gap) / 2.0 - 1.0).clamp(2.0, 14.0);
        let hovered = match self.hover {
            Some(Hit::Bar(i)) if i < n => i,
            _ => n - 1,
        };
        for (i, (d, u, _)) in pv.bars.iter().enumerate() {
            let sx = ax + i as f32 * slot;
            if i == hovered {
                g.fill_rrect(Rect::new(sx + 1.0, ay, slot - 2.0, ah), 6.0, t.hover);
            }
            let alpha = if i == hovered { 1.0 } else { 0.8 };
            let mid = sx + slot / 2.0;
            for (k, (v, c)) in [(*d, t.accent), (*u, t.green)].into_iter().enumerate() {
                if v == 0 {
                    continue;
                }
                let bh = (v as f32 / y_max as f32 * (ah - 4.0)).max(3.0);
                let bx = if k == 0 { mid - bw - 1.0 } else { mid + 1.0 };
                g.fill_rrect(Rect::new(bx, ay + ah - bh, bw, bh), (bw / 2.0).min(4.0), c.alpha(alpha));
            }
            self.hits.push((Rect::new(sx, ay, slot, ah), Hit::Bar(i)));
        }
        let lh11 = g.normal_lh(11.0);
        g.text(&pv.axis[0], 11.0).color(t.text3).draw(ax, ay + ah + 8.0);
        g.text(&pv.axis[1], 11.0)
            .color(t.text3)
            .width(120.0)
            .align(Align::Center)
            .draw(ax + aw / 2.0 - 60.0, ay + ah + 8.0);
        g.text(&pv.axis[2], 11.0).color(t.text3).width(120.0).align(Align::Right).draw(ax + aw - 120.0, ay + ah + 8.0);
        let _ = lh11;
        if let Some((d, u, label)) = pv.bars.get(hovered) {
            let center = ax + (hovered as f32 + 0.5) * slot;
            let tw = 160.0;
            let left = if center < ax + aw / 2.0 { center + 14.0 } else { center - 14.0 - tw };
            let tip = Rect::new(left.clamp(ax, ax + aw - tw), ay + 6.0, tw, 64.0);
            g.shadow(tip, 10.0, 6.0, 18.0, Color(0.0, 0.0, 0.0, 0.28));
            g.fill_rrect(tip, 10.0, t.tip_bg);
            g.text(label, 12.0)
                .weight(600)
                .color(t.tip_text)
                .width(tw - 20.0)
                .ellipsis()
                .draw(tip.x + 10.0, tip.y + 8.0);
            for (k, (name, v)) in
                [(tt("Скачано", "Downloaded"), *d), (tt("Роздано", "Shared"), *u)].into_iter().enumerate()
            {
                let yy = tip.y + 26.0 + k as f32 * 17.0;
                g.text(name, 12.0).color(t.tip_text).draw(tip.x + 10.0, yy);
                g.text(&fmt::size(v, precise), 12.0)
                    .weight(600)
                    .color(t.tip_text)
                    .width(80.0)
                    .align(Align::Right)
                    .draw(tip.x + tw - 90.0, yy);
            }
        }

        // Most uploaded torrents.
        let list =
            Rect::new(PAD, chart.y + chart.h + 12.0, w - 2.0 * PAD, (h - PAD - (chart.y + chart.h + 12.0)).max(90.0));
        g.fill_rrect(list, 16.0, t.card);
        g.stroke_rrect(list.inset(0.5), 15.5, 1.0, t.card_border);
        g.text(tt("Больше всего роздано", "Most shared"), 15.0)
            .weight(600)
            .color(t.text)
            .draw(list.x + 18.0, list.y + 12.0);
        let reset = tt("Сбросить статистику", "Reset statistics");
        let rw = g.text(reset, 12.0).weight(600).measure();
        let rr = Rect::new(list.x + list.w - 18.0 - rw - 8.0, list.y + 8.0, rw + 16.0, 28.0);
        self.button_bg(g, Hit::StatsReset, rr, 8.0);
        g.text(reset, 12.0).weight(600).color(t.link).draw(rr.x + 8.0, rr.y + (28.0 - g.normal_lh(12.0)) / 2.0);
        self.hits.push((rr, Hit::StatsReset));
        let rows = (((list.h - 46.0) / 44.0).floor() as usize).clamp(1, 5);
        let top = stats.top(rows);
        if top.is_empty() {
            g.text(tt("Пока ничего не роздано", "Nothing shared yet"), 13.0)
                .color(t.text3)
                .draw(list.x + 18.0, list.y + 50.0);
        }
        let max_up = top.first().map_or(1, |e| e.up.max(1));
        for (k, e) in top.iter().enumerate() {
            let ry = list.y + 42.0 + k as f32 * 44.0;
            let x = list.x + 10.0;
            let rank = Rect::new(x + 8.0, ry + 11.0, 22.0, 22.0);
            g.fill_rrect(rank, 7.0, t.surface2);
            g.text(&(k + 1).to_string(), 12.0)
                .weight(600)
                .color(t.text2)
                .width(22.0)
                .align(Align::Center)
                .draw(rank.x, rank.y + (22.0 - g.normal_lh(12.0)) / 2.0);
            let ratio = format!("×{}", fmt::ratio(e.ratio));
            let rw = g.text(&ratio, 12.0).weight(600).measure() + 16.0;
            let chip = Rect::new(list.x + list.w - 18.0 - rw.max(36.0), ry + 11.0, rw.max(36.0), 22.0);
            g.fill_rrect(chip, 7.0, t.green_soft);
            g.text(&ratio, 12.0)
                .weight(600)
                .color(t.green)
                .width(chip.w)
                .align(Align::Center)
                .draw(chip.x, chip.y + (22.0 - g.normal_lh(12.0)) / 2.0);
            let amount = fmt::size(e.up, precise);
            g.text(&amount, 13.0)
                .weight(600)
                .color(t.text)
                .width(70.0)
                .align(Align::Right)
                .draw(chip.x - 12.0 - 70.0, ry + (44.0 - g.normal_lh(13.0)) / 2.0);
            let nx = rank.x + rank.w + 12.0;
            let nw = chip.x - 12.0 - 70.0 - 12.0 - nx;
            g.text(&e.name, 14.0).color(t.text).width(nw).ellipsis().draw(nx, ry + 6.0);
            let track = Rect::new(nx, ry + 30.0, nw, 4.0);
            g.fill_rrect(track, 2.0, t.track);
            g.fill_rrect(Rect { w: (nw * e.up as f32 / max_up as f32).max(4.0), ..track }, 2.0, t.green);
        }
        g.pop_clip_rect();
    }

    fn paint_notice(&self, g: &Gfx, w: f32, h: f32) {
        let Some((text, _)) = &self.notice else { return };
        let t = self.theme;
        let tw = g.text(text, 13.0).weight(500).measure().min(w - 80.0);
        let bottom = if self.view == View::List { h - PAD - SUMMARY_H - 12.0 } else { h - 20.0 };
        let r = Rect::new((w - tw - 32.0) / 2.0, bottom - 36.0, tw + 32.0, 36.0);
        g.shadow(r, 18.0, 4.0, 14.0, Color(0.0, 0.0, 0.0, 0.18));
        g.fill_rrect(r, 18.0, t.tip_bg);
        g.text(text, 13.0)
            .weight(500)
            .color(t.tip_text)
            .width(tw)
            .ellipsis()
            .draw(r.x + 16.0, r.y + (36.0 - g.normal_lh(13.0)) / 2.0);
    }

    /// A one-line text field with placeholder and caret. The caller
    /// registers the hit.
    fn text_field(&self, g: &Gfx, r: Rect, text: &str, placeholder: &str, focused: bool, left_pad: f32) {
        let t = self.theme;
        let ty = r.y + (r.h - g.normal_lh(14.0)) / 2.0;
        let avail = r.w - left_pad - 12.0;
        if text.is_empty() {
            g.text(placeholder, 14.0).color(t.text3).width(avail).ellipsis().draw(r.x + left_pad, ty);
        }
        let shown = if text.is_empty() {
            0.0
        } else {
            g.text(text, 14.0).color(t.text).width(avail).ellipsis().draw(r.x + left_pad, ty);
            g.text(text, 14.0).measure().min(avail)
        };
        if focused && self.caret_on {
            g.fill_rect(Rect::new(r.x + left_pad + shown + 1.0, r.y + (r.h - 20.0) / 2.0, 1.5, 20.0), t.text);
        }
    }

    fn paint_rename(&mut self, g: &Gfx, w: f32, h: f32) {
        let t = self.theme;
        let Some(rename) = &self.rename else { return };
        g.fill_rect(Rect::new(0.0, 0.0, w, h), t.scrim);
        let pw = (w - 32.0).min(460.0);
        let ph = 218.0;
        let p = Rect::new((w - pw) / 2.0, (h - ph) / 2.0 - 40.0, pw, ph);
        g.shadow(p, 16.0, 8.0, 30.0, Color(0.0, 0.0, 0.0, 0.25));
        g.fill_rrect(p, 16.0, t.sheet);
        g.stroke_rrect(p.inset(0.5), 15.5, 1.0, t.card_border);
        let mut hits = vec![(p, Hit::RenameBody)];
        let x = p.x + 22.0;
        let iw = pw - 44.0;
        g.text(tt("Переименовать", "Rename"), 12.0).color(t.text2).draw(x, p.y + 20.0);
        g.text(&tr!("В торренте — {}", "In the torrent: {}", rename.original), 15.0)
            .weight(600)
            .color(t.text)
            .width(iw)
            .ellipsis()
            .draw(x, p.y + 38.0);
        let field = Rect::new(x, p.y + 72.0, iw, 38.0);
        g.fill_rrect(field, 8.0, t.field);
        g.stroke_rrect(field.inset(0.75), 7.25, 1.5, t.accent);
        self.text_field(g, field, &rename.text, tt("Название в списке", "Name in the list"), true, 12.0);
        hits.push((field, Hit::RenameField));
        g.text(
            tt("Оставьте пустым, чтобы вернуть название из торрента", "Leave empty to use the name from the torrent"),
            12.0,
        )
        .color(t.text3)
        .width(iw)
        .ellipsis()
        .draw(x, field.y + field.h + 8.0);
        let ok = Rect::new(p.x + pw - 22.0 - 130.0, p.y + ph - 22.0 - 38.0, 130.0, 38.0);
        let cancel = Rect::new(ok.x - 10.0 - 110.0, ok.y, 110.0, 38.0);
        for (r, label, hit, primary) in [
            (cancel, tt("Отмена", "Cancel"), Hit::RenameCancel, false),
            (ok, tt("Сохранить", "Save"), Hit::RenameOk, true),
        ] {
            let (bg, fg) = if primary { (t.accent, t.on_accent) } else { (t.chip, t.text) };
            g.fill_rrect(r, 8.0, bg);
            if !primary {
                g.stroke_rrect(r.inset(0.5), 7.5, 1.0, t.chip_border);
            }
            if self.pressed == Some(hit) {
                g.fill_rrect(r, 8.0, Color(0.0, 0.0, 0.0, 0.12));
            } else if self.hover == Some(hit) {
                g.fill_rrect(r, 8.0, if primary { Color::white(0.12) } else { t.hover });
            }
            g.text(label, 14.0)
                .weight(600)
                .color(fg)
                .width(r.w)
                .align(Align::Center)
                .draw(r.x, r.y + (r.h - g.normal_lh(14.0)) / 2.0);
            hits.push((r, hit));
        }
        self.hits.extend(hits);
    }

    // ---------------------------------------------------------------- sheet

    fn paint_sheet(&mut self, g: &Gfx, w: f32, h: f32) {
        let t = self.theme;
        g.fill_rect(Rect::new(0.0, 0.0, w, h), t.scrim);
        let pw = (w - 32.0).min(560.0);
        let ph = (h - 40.0).min(600.0);
        let p = Rect::new((w - pw) / 2.0, (h - ph) / 2.0, pw, ph);
        g.shadow(p, 16.0, 8.0, 30.0, Color(0.0, 0.0, 0.0, 0.25));
        g.fill_rrect(p, 16.0, t.sheet);
        g.stroke_rrect(p.inset(0.5), 15.5, 1.0, t.card_border);
        self.hits.push((p, Hit::SheetBody));

        let hover = self.hover;
        let pressed = self.pressed;
        let spin = self.spin;
        let caret_on = self.caret_on;
        let precise = self.precise;
        let Some(sheet) = &self.sheet else { return };
        let x = p.x + 22.0;
        let iw = pw - 44.0;
        let mut header = tt("Новая загрузка", "New download").to_string();
        if sheet.position.1 > 1 {
            header += &tr!(" · {} из {}", " · {} of {}", sheet.position.0, sheet.position.1);
        }
        g.text(&header, 12.0).color(t.text2).draw(x, p.y + 20.0);
        let name = match &sheet.state {
            SheetState::Ready(d) => d.name.clone(),
            _ => short_source(&sheet.source),
        };
        g.text(&name, 18.0).weight(650).color(t.text).width(iw).ellipsis().draw(x, p.y + 38.0);

        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let button =
            |g: &Gfx, hits: &mut Vec<(Rect, Hit)>, r: Rect, label: &str, hit: Hit, primary: bool, enabled: bool| {
                let (bg, fg) = if primary { (t.accent, t.on_accent) } else { (t.chip, t.text) };
                let bg = if !enabled { bg.alpha(0.4) } else { bg };
                g.fill_rrect(r, 8.0, bg);
                if !primary {
                    g.stroke_rrect(r.inset(0.5), 7.5, 1.0, t.chip_border);
                }
                if enabled && pressed == Some(hit) {
                    g.fill_rrect(r, 8.0, Color(0.0, 0.0, 0.0, 0.12));
                } else if enabled && hover == Some(hit) {
                    g.fill_rrect(r, 8.0, if primary { Color::white(0.12) } else { t.hover });
                }
                g.text(label, 14.0)
                    .weight(600)
                    .color(if enabled { fg } else { fg.alpha(0.6) })
                    .width(r.w)
                    .align(Align::Center)
                    .draw(r.x, r.y + (r.h - g.normal_lh(14.0)) / 2.0);
                if enabled {
                    hits.push((r, hit));
                }
            };
        let footer_y = p.y + ph - 22.0 - 38.0;
        let ok = Rect::new(p.x + pw - 22.0 - 130.0, footer_y, 130.0, 38.0);
        let cancel = Rect::new(ok.x - 10.0 - 110.0, footer_y, 110.0, 38.0);

        match &sheet.state {
            SheetState::Loading => {
                let cy = p.y + ph / 2.0 - 20.0;
                let cx = p.x + pw / 2.0;
                g.stroke_circle(cx, cy, 16.0, 3.0, t.track);
                g.push_transform(crate::gfx::mul(
                    crate::gfx::mul(crate::gfx::translate(-cx, -cy), rotation(spin)),
                    crate::gfx::translate(cx, cy),
                ));
                g.ring(cx, cy, 16.0, 3.0, 28.0, t.accent, Color(0.0, 0.0, 0.0, 0.0));
                g.pop_transform();
                g.text(tt("Получаю список файлов…", "Getting the file list…"), 14.0)
                    .color(t.text2)
                    .width(pw)
                    .align(Align::Center)
                    .draw(p.x, cy + 30.0);
                button(g, &mut hits, ok, tt("Отмена", "Cancel"), Hit::SheetCancel, false, true);
            }
            SheetState::Failed(msg) => {
                let cy = p.y + 100.0;
                icons::draw(g, icons::WARN, x, cy, 24.0, 1.8, t.red);
                g.text(msg, 14.0).color(t.text).width(iw - 36.0).wrap().draw(x + 36.0, cy + 2.0);
                button(g, &mut hits, ok, tt("Закрыть", "Close"), Hit::SheetCancel, false, true);
            }
            SheetState::Ready(draft) => {
                // Alias field.
                let mut y = p.y + 78.0;
                g.text(tt("Своё название", "Custom name"), 12.0).color(t.text2).draw(x, y);
                let field = Rect::new(x, y + 20.0, iw, 38.0);
                g.fill_rrect(field, 8.0, t.field);
                if sheet.alias_focus {
                    g.stroke_rrect(field.inset(0.75), 7.25, 1.5, t.accent);
                } else {
                    g.stroke_rrect(field.inset(0.5), 7.5, 1.0, t.chip_border);
                }
                let ty = field.y + (38.0 - g.normal_lh(14.0)) / 2.0;
                if sheet.alias.is_empty() {
                    g.text(
                        tt("Необязательно — как подписать загрузку в списке", "Optional — how to label it in the list"),
                        14.0,
                    )
                    .color(t.text3)
                    .width(iw - 24.0)
                    .ellipsis()
                    .draw(x + 12.0, ty);
                }
                let shown_w = if sheet.alias.is_empty() {
                    0.0
                } else {
                    g.text(&sheet.alias, 14.0).color(t.text).width(iw - 24.0).ellipsis().draw(x + 12.0, ty);
                    g.text(&sheet.alias, 14.0).measure().min(iw - 24.0)
                };
                if sheet.alias_focus && caret_on {
                    g.fill_rect(Rect::new(x + 12.0 + shown_w + 1.0, field.y + 9.0, 1.5, 20.0), t.text);
                }
                hits.push((field, Hit::SheetAlias));
                y = field.y + field.h + 18.0;

                // Files header.
                let n = sheet.selected_count();
                let head = tr!(
                    "Файлы · выбрано {} из {} · {}",
                    "Files · {} of {} selected · {}",
                    n,
                    draft.files.len(),
                    fmt::size(sheet.selected_bytes(), precise)
                );
                g.text(&head, 13.0).weight(600).color(t.text).width(iw - 120.0).ellipsis().draw(x, y);
                let all_label = if n == draft.files.len() {
                    tt("Снять все", "Select none")
                } else {
                    tt("Выбрать все", "Select all")
                };
                let aw = g.text(all_label, 13.0).weight(600).measure();
                let all = Rect::new(x + iw - aw - 12.0, y - 4.0, aw + 12.0, 26.0);
                if hover == Some(Hit::SheetAll) {
                    g.fill_rrect(all, 6.0, t.hover);
                }
                g.text(all_label, 13.0).weight(600).color(t.accent).draw(all.x + 6.0, y);
                hits.push((all, Hit::SheetAll));
                y += 28.0;

                // File list.
                let list_bottom = footer_y - 20.0 - 52.0;
                let view = Rect::new(x - 6.0, y, iw + 12.0, (list_bottom - y).max(40.0));
                g.fill_rrect(view, 10.0, t.field);
                let row_h = 34.0;
                let content_h = draft.files.len() as f32 * row_h + 8.0;
                let max = (content_h - view.h).max(0.0);
                let fscroll = sheet.file_scroll.min(max);
                g.push_clip_rrect(view, 10.0);
                let mut ry = view.y + 4.0 - fscroll;
                for (k, f) in draft.files.iter().enumerate() {
                    if ry + row_h >= view.y && ry <= view.y + view.h {
                        let row = Rect::new(view.x + 4.0, ry, view.w - 8.0, row_h);
                        if hover == Some(Hit::SheetFile(k)) {
                            g.fill_rrect(row, 7.0, t.hover);
                        }
                        let cb = Rect::new(row.x + 8.0, ry + 8.0, 18.0, 18.0);
                        if sheet.checked[k] {
                            g.fill_rrect(cb, 5.0, t.accent);
                            icons::draw(g, icons::CHECK, cb.x + 1.0, cb.y + 1.0, 16.0, 2.2, t.on_accent);
                        } else {
                            g.stroke_rrect(cb.inset(0.75), 4.25, 1.5, t.text3);
                        }
                        let size = fmt::size(f.size, precise);
                        let sw = 86.0;
                        let ty = ry + (row_h - g.normal_lh(13.0)) / 2.0;
                        g.text(&f.path, 13.0)
                            .color(if sheet.checked[k] { t.text } else { t.text2 })
                            .width(row.w - 36.0 - sw - 8.0)
                            .ellipsis()
                            .draw(cb.x + 28.0, ty);
                        g.text(&size, 13.0)
                            .color(t.text2)
                            .width(sw)
                            .align(Align::Right)
                            .draw(row.x + row.w - sw - 10.0, ty);
                        let visible =
                            Rect::new(row.x, ry.max(view.y), row.w, (ry + row_h).min(view.y + view.h) - ry.max(view.y));
                        hits.push((visible, Hit::SheetFile(k)));
                    }
                    ry += row_h;
                }
                g.pop_layer();
                if max > 0.0 {
                    let bh = (view.h * view.h / content_h).max(24.0);
                    let by = view.y + (view.h - bh) * (fscroll / max);
                    g.fill_rrect(Rect::new(view.x + view.w - 5.0, by + 3.0, 3.0, bh - 6.0), 1.5, t.text3.alpha(0.7));
                }
                self.files_view = view;
                self.files_content_h = content_h;
                hits.insert(0, (view, Hit::SheetFiles));

                // Folder row.
                let fy = list_bottom + 14.0;
                icons::draw(g, icons::FOLDER, x, fy, 20.0, 1.6, t.text2);
                let change = tt("Изменить", "Change");
                let cw = g.text(change, 13.0).weight(600).measure();
                let cr = Rect::new(x + iw - cw - 12.0, fy - 3.0, cw + 12.0, 26.0);
                if hover == Some(Hit::SheetFolder) {
                    g.fill_rrect(cr, 6.0, t.hover);
                }
                g.text(change, 13.0).weight(600).color(t.accent).draw(cr.x + 6.0, fy);
                hits.push((cr, Hit::SheetFolder));
                let folder = sheet.folder.to_string_lossy().into_owned();
                g.text(&folder, 13.0).color(t.text).width(cr.x - x - 36.0).ellipsis().draw(x + 28.0, fy);
                let need = sheet.selected_bytes();
                let (free_text, free_color) = match sheet.free {
                    Some(free) if free < need => (
                        tr!(
                            "Не хватает места: нужно {}, свободно {}",
                            "Not enough space: need {}, free {}",
                            fmt::size(need, false),
                            fmt::size(free, false)
                        ),
                        t.red,
                    ),
                    Some(free) => (tr!("Свободно {}", "Free: {}", fmt::size(free, false)), t.text2),
                    None => (String::new(), t.text2),
                };
                g.text(&free_text, 12.0).color(free_color).width(iw - 28.0).ellipsis().draw(x + 28.0, fy + 22.0);

                let can = sheet.can_start();
                button(g, &mut hits, cancel, tt("Отмена", "Cancel"), Hit::SheetCancel, false, true);
                button(
                    g,
                    &mut hits,
                    ok,
                    if sheet.busy { tt("Добавляю…", "Adding…") } else { tt("Скачать", "Download") },
                    Hit::SheetOk,
                    true,
                    can,
                );
            }
        }
        self.hits.extend(hits);
    }
}

fn rotation(deg: f32) -> windows_numerics::Matrix3x2 {
    let (s, c) = deg.to_radians().sin_cos();
    windows_numerics::Matrix3x2 { M11: c, M12: s, M21: -s, M22: c, M31: 0.0, M32: 0.0 }
}

fn is_compact(s: &Snapshot) -> bool {
    matches!(s.status, Status::Finished | Status::Seeding)
}

fn status_label(s: &Snapshot) -> &'static str {
    match s.status {
        Status::Checking => tt("Проверка", "Checking"),
        Status::Downloading if s.peers == 0 => tt("Ищу пиров", "Finding peers"),
        Status::Downloading => tt("Загрузка", "Downloading"),
        Status::Seeding => tt("Раздаётся", "Seeding"),
        Status::Paused => tt("Пауза", "Paused"),
        Status::Finished => tt("Готово", "Done"),
        Status::Error => tt("Ошибка", "Error"),
    }
}

fn status_look(t: &Theme, s: &Snapshot) -> (Color, &'static str) {
    match s.status {
        Status::Checking => (t.text2, icons::CLOCK),
        Status::Downloading => (t.accent, icons::DOWN),
        Status::Seeding => (t.green, icons::UP),
        Status::Paused => (t.text2, icons::PAUSE),
        Status::Finished => (t.green, icons::DONE),
        Status::Error => (t.red, icons::WARN),
    }
}

/// Speed levels: under 1 MB/s neutral, up to 10 MB/s green, faster orange.
fn speed_color(t: &Theme, bps: u64) -> Color {
    const MB: u64 = 1024 * 1024;
    match bps {
        0..MB => t.text2,
        MB..10_485_760 => t.green,
        _ => t.orange,
    }
}

fn short_source(s: &str) -> String {
    if let Some(dn) = s.split(['?', '&']).find_map(|p| p.strip_prefix("dn=")) {
        return dn.replace('+', " ").replace("%20", " ");
    }
    if s.starts_with("magnet:") {
        return tt("Magnet-ссылка", "Magnet link").into();
    }
    std::path::Path::new(s).file_name().map_or_else(|| s.to_string(), |n| n.to_string_lossy().into_owned())
}

/// A rounded-up scale for the chart's y axis: (max in bytes, labels top..0).
fn axis_scale(max: u64) -> (u64, [String; 3]) {
    const UNITS: [(u64, usize); 4] = [(1 << 40, 4), (1 << 30, 3), (1 << 20, 2), (1 << 10, 1)];
    let (unit, idx) = UNITS.iter().copied().find(|(u, _)| max >= *u).unwrap_or((1 << 30, 3));
    let name = fmt::unit(idx);
    let v = (max as f64 / unit as f64).max(1e-9);
    let mag = 10f64.powf(v.log10().floor());
    let nice = [1.0, 2.0, 4.0, 5.0, 10.0].into_iter().map(|m| m * mag).find(|n| *n >= v - 1e-9).unwrap_or(10.0 * mag);
    let nice = if max == 0 { 1.0 } else { nice };
    let label = |x: f64| {
        let d = if x.fract().abs() < 1e-9 { 0 } else { 1 };
        format!("{} {name}", fmt::num(x, d))
    };
    ((nice * unit as f64) as u64, [label(nice), label(nice / 2.0), "0".to_string()])
}
