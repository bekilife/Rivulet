//! What the window shows: toolbar, download cards, the "new download" sheet.
//! Immediate mode: every paint lays everything out again and records the
//! clickable rectangles in `hits` for the next mouse event.

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
}

impl Hit {
    pub fn clickable(self) -> bool {
        !matches!(self, Hit::SheetBody | Hit::SheetFiles | Hit::RenameBody)
    }
    pub fn is_text_field(self) -> bool {
        matches!(self, Hit::SheetAlias | Hit::SearchField | Hit::RenameField)
    }
}

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
    content_h: f32,
    view_h: f32,
    files_view: Rect,
    files_content_h: f32,
}

const PAD: f32 = 16.0;
const TOOLBAR_H: f32 = 68.0;
const GAP: f32 = 10.0;

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
            content_h: 0.0,
            view_h: 0.0,
            files_view: Rect::default(),
            files_content_h: 0.0,
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
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
        let max = (self.content_h - self.view_h).max(0.0);
        self.scroll = (self.scroll - delta).clamp(0.0, max);
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

    pub fn paint(&mut self, g: &Gfx, w: f32, h: f32) {
        self.hits.clear();
        let t = self.theme;
        g.fill_rect(Rect::new(0.0, 0.0, w, h), t.backdrop);

        self.paint_list(g, w, h);
        self.paint_toolbar(g, w);
        if self.notice_active() {
            self.paint_notice(g, w, h);
        }
        if self.sheet.is_some() {
            // The sheet takes all input while it is open.
            self.hits.clear();
            self.paint_sheet(g, w, h);
        } else if self.rename.is_some() {
            self.hits.clear();
            self.paint_rename(g, w, h);
        }
    }

    /// A one-line text field with placeholder and caret. Returns nothing;
    /// the caller registers the hit.
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
        g.text("Переименовать", 12.0).color(t.text2).draw(x, p.y + 20.0);
        g.text(&format!("В торренте — {}", rename.original), 15.0)
            .weight(600)
            .color(t.text)
            .width(iw)
            .ellipsis()
            .draw(x, p.y + 38.0);
        let field = Rect::new(x, p.y + 72.0, iw, 38.0);
        g.fill_rrect(field, 8.0, t.field);
        g.stroke_rrect(field.inset(0.75), 7.25, 1.5, t.accent);
        self.text_field(g, field, &rename.text, "Название в списке", true, 12.0);
        hits.push((field, Hit::RenameField));
        g.text("Оставьте пустым, чтобы вернуть название из торрента", 12.0)
            .color(t.text3)
            .width(iw)
            .ellipsis()
            .draw(x, field.y + field.h + 8.0);
        let ok = Rect::new(p.x + pw - 22.0 - 130.0, p.y + ph - 22.0 - 38.0, 130.0, 38.0);
        let cancel = Rect::new(ok.x - 10.0 - 110.0, ok.y, 110.0, 38.0);
        for (r, label, hit, primary) in
            [(cancel, "Отмена", Hit::RenameCancel, false), (ok, "Сохранить", Hit::RenameOk, true)]
        {
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

    fn button_bg(&self, g: &Gfx, hit: Hit, r: Rect, rad: f32) {
        if self.pressed == Some(hit) {
            g.fill_rrect(r, rad, self.theme.press);
        } else if self.hover == Some(hit) {
            g.fill_rrect(r, rad, self.theme.hover);
        }
    }

    fn paint_toolbar(&mut self, g: &Gfx, w: f32) {
        let t = self.theme;
        let group = Rect::new(PAD, 12.0, 3.0 * 40.0 + 8.0, 44.0);
        g.fill_rrect(group, 22.0, t.card);
        g.stroke_rrect(group.inset(0.5), 21.5, 1.0, t.card_border);
        let any = !self.selected.is_empty();
        let toggle_icon = if self.toggle_pauses() { icons::PAUSE } else { icons::PLAY };
        let items = [(Hit::Add, icons::PLUS, true), (Hit::Toggle, toggle_icon, any), (Hit::Remove, icons::TRASH, any)];
        for (i, (hit, icon, enabled)) in items.into_iter().enumerate() {
            let r = Rect::new(group.x + 4.0 + i as f32 * 40.0, group.y + 4.0, 40.0, 36.0);
            if enabled {
                self.button_bg(g, hit, r, 18.0);
                self.hits.push((r, hit));
            }
            let c = if enabled { t.text } else { t.text3 };
            icons::draw(g, icon, r.x + 9.0, r.y + 7.0, 22.0, 1.7, c);
        }
        let s = Rect::new(w - PAD - 44.0, 12.0, 44.0, 44.0);

        if let Some(q) = &self.search {
            // Search field between the button group and settings.
            let f = Rect::new(group.x + group.w + 10.0, 12.0, s.x - 10.0 - (group.x + group.w + 10.0), 44.0);
            g.fill_rrect(f, 22.0, t.card);
            if self.search_focus {
                g.stroke_rrect(f.inset(0.75), 21.25, 1.5, t.accent);
            } else {
                g.stroke_rrect(f.inset(0.5), 21.5, 1.0, t.card_border);
            }
            icons::draw(g, icons::SEARCH, f.x + 13.0, f.y + 12.0, 20.0, 1.7, t.text2);
            let field = Rect::new(f.x, f.y, f.w - 40.0, f.h);
            self.text_field(g, field, q, "Поиск по названию", self.search_focus, 42.0);
            self.hits.push((f, Hit::SearchField));
            let c = Rect::new(f.x + f.w - 40.0, f.y + 6.0, 32.0, 32.0);
            self.button_bg(g, Hit::SearchClear, c, 16.0);
            icons::draw(g, icons::CLOSE, c.x + 7.0, c.y + 7.0, 18.0, 1.7, t.text2);
            self.hits.push((c, Hit::SearchClear));
        } else {
            g.text("Rivulet", 18.0)
                .weight(650)
                .color(t.text)
                .draw(group.x + group.w + 14.0, 12.0 + (44.0 - g.normal_lh(18.0)) / 2.0);
            let b = Rect::new(s.x - 10.0 - 44.0, 12.0, 44.0, 44.0);
            g.fill_rrect(b, 22.0, t.card);
            g.stroke_rrect(b.inset(0.5), 21.5, 1.0, t.card_border);
            self.button_bg(g, Hit::Search, b.inset(4.0), 18.0);
            self.hits.push((b, Hit::Search));
            icons::draw(g, icons::SEARCH, b.x + 11.0, b.y + 11.0, 22.0, 1.7, t.text);
        }

        g.fill_rrect(s, 22.0, t.card);
        g.stroke_rrect(s.inset(0.5), 21.5, 1.0, t.card_border);
        self.button_bg(g, Hit::Settings, s.inset(4.0), 18.0);
        self.hits.push((s, Hit::Settings));
        icons::draw(g, icons::SLIDERS, s.x + 11.0, s.y + 11.0, 22.0, 1.7, t.text);
    }

    fn card_height(&self, s: &Snapshot) -> f32 {
        let expanded = self.expanded.contains(&s.info_hash);
        if is_compact(s) && !expanded {
            return 64.0;
        }
        let mut h = 16.0 + 22.0 + 12.0 + 6.0 + 12.0 + 32.0 + 16.0;
        if s.alias.is_some() || s.error.is_some() {
            h += 24.0;
        }
        if expanded {
            h += 14.0 + 18.0 + 6.0 + 32.0 + 4.0;
        }
        h
    }

    fn paint_list(&mut self, g: &Gfx, w: f32, h: f32) {
        let t = self.theme;
        let top = TOOLBAR_H;
        self.view_h = h - top;
        if self.snaps.is_empty() {
            self.content_h = 0.0;
            self.scroll = 0.0;
            let cy = top + (h - top) / 2.0 - 50.0;
            g.text("Rivulet", 44.0).weight(700).color(t.accent).width(w).align(Align::Center).draw(0.0, cy);
            g.text("Перетащите сюда .torrent-файл, нажмите «+» или вставьте magnet-ссылку (Ctrl+V)", 14.0)
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
            g.text(&format!("Ничего не найдено по запросу «{}»", q.trim()), 14.0)
                .color(t.text2)
                .width(w - 64.0)
                .align(Align::Center)
                .ellipsis()
                .draw(32.0, top + 40.0);
            return;
        }
        let heights: Vec<f32> = shown.iter().map(|&i| self.card_height(&self.snaps[i])).collect();
        self.content_h = heights.iter().sum::<f32>() + GAP * heights.len() as f32 + 8.0;
        let max = (self.content_h - self.view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);

        g.push_clip_rect(Rect::new(0.0, top, w, h - top));
        let mut y = top + 2.0 - self.scroll;
        for (k, &i) in shown.iter().enumerate() {
            let ch = heights[k];
            if y + ch >= top && y <= h {
                let r = Rect::new(PAD, y, w - 2.0 * PAD, ch);
                self.paint_card(g, i, r);
            }
            y += ch + GAP;
        }
        g.pop_clip_rect();
        // Keep hits inside the visible list area.
        self.hits.retain(|(r, hit)| !matches!(hit, Hit::Card(_) | Hit::Expand(_)) || r.y + r.h > top);

        if max > 0.0 {
            let bar_h = (self.view_h * self.view_h / self.content_h).max(30.0);
            let bar_y = top + (self.view_h - bar_h) * (self.scroll / max);
            g.fill_rrect(Rect::new(w - 6.0, bar_y + 2.0, 3.0, bar_h - 4.0), 1.5, t.text3.alpha(0.6));
        }
    }

    fn paint_card(&mut self, g: &Gfx, i: usize, r: Rect) {
        let t = self.theme;
        let s = &self.snaps[i];
        let selected = self.selected.contains(&s.info_hash);
        let expanded = self.expanded.contains(&s.info_hash);
        let hovered = matches!(self.hover, Some(Hit::Card(j) | Hit::Expand(j)) if j == i);
        g.fill_rrect(r, 14.0, t.card);
        if selected {
            g.stroke_rrect(r.inset(0.75), 13.25, 1.5, t.text);
        } else {
            g.stroke_rrect(r.inset(0.5), 13.5, 1.0, if hovered { t.card_hover_border } else { t.card_border });
        }
        self.hits.push((r, Hit::Card(i)));

        let title = s.alias.clone().unwrap_or_else(|| s.name.clone());
        let precise = self.precise;
        let (status_color, status_icon) = status_look(&t, s);

        // Expand button (top right) and selection mark.
        let eb = Rect::new(r.x + r.w - 12.0 - 30.0, r.y + 12.0, 30.0, 30.0);
        let compact = is_compact(s) && !expanded;
        let eb = if compact { Rect::new(eb.x, r.y + 17.0, 30.0, 30.0) } else { eb };
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
            icons::draw(
                g,
                icons::CHECK,
                cx - 7.0,
                eb.y + 8.0,
                14.0,
                2.0,
                if t.dark { Color::hex(0x1A1A1A) } else { Color::white(1.0) },
            );
            right = cx - 18.0;
        }

        if compact {
            let pill = Rect::new(r.x + 14.0, r.y + 14.0, 36.0, 36.0);
            g.fill_rrect(pill, 10.0, status_color.alpha(0.14));
            icons::draw(g, status_icon, pill.x + 7.0, pill.y + 7.0, 22.0, 1.8, status_color);
            let label = status_label(s);
            let lw = g.text(label, 13.0).measure();
            g.text(label, 13.0).color(t.text2).draw(right - lw, r.y + (64.0 - g.normal_lh(13.0)) / 2.0);
            g.text(&title, 15.0)
                .weight(600)
                .color(t.text)
                .width(right - lw - 12.0 - (pill.x + 48.0))
                .ellipsis()
                .draw(pill.x + 48.0, r.y + (64.0 - g.normal_lh(15.0)) / 2.0);
            return;
        }

        let x = r.x + 16.0;
        let inner_w = r.w - 32.0;
        g.text(&title, 15.0).weight(600).color(t.text).width(right - x - 8.0).ellipsis().draw(x, r.y + 16.0);

        // Progress bar.
        let bar = Rect::new(x, r.y + 50.0, inner_w, 6.0);
        g.fill_rrect(bar, 3.0, t.track);
        let fill_w = (bar.w * s.progress as f32).clamp(0.0, bar.w);
        if fill_w > 0.5 {
            g.fill_rrect(Rect::new(bar.x, bar.y, fill_w.max(6.0), bar.h), 3.0, status_color);
        }

        // Status row: [icon 25%] Загрузка ........ [speed | eta]
        let row_y = r.y + 68.0;
        let pct = fmt::percent(s.progress, precise);
        let pw = g.text(&pct, 14.0).weight(600).measure();
        let pill = Rect::new(x, row_y, 10.0 + 18.0 + 6.0 + pw + 12.0, 32.0);
        g.fill_rrect(pill, 9.0, status_color.alpha(0.14));
        icons::draw(g, status_icon, pill.x + 10.0, pill.y + 7.0, 18.0, 1.8, status_color);
        g.text(&pct, 14.0)
            .weight(600)
            .color(status_color)
            .draw(pill.x + 34.0, row_y + (32.0 - g.normal_lh(14.0)) / 2.0);
        g.text(status_label(s), 14.0)
            .weight(500)
            .color(t.text)
            .draw(pill.x + pill.w + 10.0, row_y + (32.0 - g.normal_lh(14.0)) / 2.0);

        if matches!(s.status, Status::Downloading | Status::Seeding) {
            let (speed_bps, icon) =
                if s.status == Status::Seeding { (s.up_bps, icons::UP) } else { (s.down_bps, icons::DOWN) };
            let sp = fmt::speed(speed_bps, precise);
            let spw = g.text(&sp, 14.0).weight(600).measure();
            let eta = s.eta_secs.map(fmt::eta);
            let etw = eta.as_ref().map_or(0.0, |e| g.text(e, 14.0).measure() + 24.0);
            let chip = Rect::new(x + inner_w - (8.0 + 18.0 + 6.0 + spw + 12.0 + etw + 4.0), row_y, 0.0, 32.0);
            let chip = Rect { w: x + inner_w - chip.x, ..chip };
            g.stroke_rrect(chip.inset(0.5), 9.5, 1.0, t.chip_border);
            let level = speed_color(&t, speed_bps);
            let inner = Rect::new(chip.x + 3.0, chip.y + 3.0, 8.0 + 18.0 + 6.0 + spw + 10.0, 26.0);
            g.fill_rrect(inner, 7.0, level.alpha(0.14));
            icons::draw(g, icon, inner.x + 7.0, inner.y + 4.0, 18.0, 1.8, level);
            g.text(&sp, 14.0).weight(600).color(level).draw(inner.x + 30.0, row_y + (32.0 - g.normal_lh(14.0)) / 2.0);
            if let Some(e) = &eta {
                g.text(e, 14.0).color(t.text).draw(inner.x + inner.w + 12.0, row_y + (32.0 - g.normal_lh(14.0)) / 2.0);
            }
        }

        let mut y = row_y + 32.0 + 10.0;
        if let Some(err) = &s.error {
            g.text(err, 13.0).color(t.red).width(inner_w).ellipsis().draw(x, y);
            y += 24.0;
        } else if s.alias.is_some() {
            let sub = format!("В торренте — {}", s.name);
            g.text(&sub, 13.0).color(t.text2).width(inner_w).ellipsis().draw(x, y);
            y += 24.0;
        }

        if expanded {
            g.fill_rect(Rect::new(x, y + 4.0, inner_w, 1.0), t.separator);
            let ly = y + 16.0;
            let vy = ly + 22.0;
            let cols: [(&str, &'static str, String); 4] = [
                ("Сиды и пиры", icons::PEERS, format!("{} · {}", s.seeds, s.peers)),
                ("Отдача", icons::UP, fmt::speed(s.up_bps, precise)),
                ("Отдано", icons::ROTATE, fmt::size(s.uploaded, precise)),
                ("Размер", icons::BOX, fmt::size(s.size, precise)),
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

    fn paint_notice(&self, g: &Gfx, w: f32, h: f32) {
        let Some((text, _)) = &self.notice else { return };
        let t = self.theme;
        let tw = g.text(text, 13.0).weight(500).measure().min(w - 80.0);
        let r = Rect::new((w - tw - 32.0) / 2.0, h - 56.0, tw + 32.0, 36.0);
        let (bg, fg) = if t.dark {
            (Color::hex(0xF2F2F2), Color::hex(0x1A1A1A))
        } else {
            (Color::hex(0x2B2B2B), Color::white(1.0))
        };
        g.shadow(r, 18.0, 4.0, 14.0, Color(0.0, 0.0, 0.0, 0.18));
        g.fill_rrect(r, 18.0, bg);
        g.text(text, 13.0)
            .weight(500)
            .color(fg)
            .width(tw)
            .ellipsis()
            .draw(r.x + 16.0, r.y + (36.0 - g.normal_lh(13.0)) / 2.0);
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
        let mut header = "Новая загрузка".to_string();
        if sheet.position.1 > 1 {
            header += &format!(" · {} из {}", sheet.position.0, sheet.position.1);
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
                g.text("Получаю список файлов…", 14.0)
                    .color(t.text2)
                    .width(pw)
                    .align(Align::Center)
                    .draw(p.x, cy + 30.0);
                button(g, &mut hits, ok, "Отмена", Hit::SheetCancel, false, true);
            }
            SheetState::Failed(msg) => {
                let cy = p.y + 100.0;
                icons::draw(g, icons::WARN, x, cy, 24.0, 1.8, t.red);
                g.text(msg, 14.0).color(t.text).width(iw - 36.0).wrap().draw(x + 36.0, cy + 2.0);
                button(g, &mut hits, ok, "Закрыть", Hit::SheetCancel, false, true);
            }
            SheetState::Ready(draft) => {
                // Alias field.
                let mut y = p.y + 78.0;
                g.text("Своё название", 12.0).color(t.text2).draw(x, y);
                let field = Rect::new(x, y + 20.0, iw, 38.0);
                g.fill_rrect(field, 8.0, t.field);
                if sheet.alias_focus {
                    g.stroke_rrect(field.inset(0.75), 7.25, 1.5, t.accent);
                } else {
                    g.stroke_rrect(field.inset(0.5), 7.5, 1.0, t.chip_border);
                }
                let ty = field.y + (38.0 - g.normal_lh(14.0)) / 2.0;
                if sheet.alias.is_empty() {
                    g.text("Необязательно — как подписать загрузку в списке", 14.0)
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
                let head = format!(
                    "Файлы · выбрано {} из {} · {}",
                    n,
                    draft.files.len(),
                    fmt::size(sheet.selected_bytes(), precise)
                );
                g.text(&head, 13.0).weight(600).color(t.text).width(iw - 120.0).ellipsis().draw(x, y);
                let all_label = if n == draft.files.len() { "Снять все" } else { "Выбрать все" };
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
                let change = "Изменить";
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
                        format!(
                            "Не хватает места: нужно {}, свободно {}",
                            fmt::size(need, false),
                            fmt::size(free, false)
                        ),
                        t.red,
                    ),
                    Some(free) => (format!("Свободно {}", fmt::size(free, false)), t.text2),
                    None => (String::new(), t.text2),
                };
                g.text(&free_text, 12.0).color(free_color).width(iw - 28.0).ellipsis().draw(x + 28.0, fy + 22.0);

                let can = sheet.can_start();
                button(g, &mut hits, cancel, "Отмена", Hit::SheetCancel, false, true);
                button(g, &mut hits, ok, if sheet.busy { "Добавляю…" } else { "Скачать" }, Hit::SheetOk, true, can);
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
        Status::Checking => "Проверка",
        Status::Downloading if s.peers == 0 => "Ищу пиров",
        Status::Downloading => "Загрузка",
        Status::Seeding => "Раздаётся",
        Status::Paused => "Пауза",
        Status::Finished => "Готово",
        Status::Error => "Ошибка",
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
        return "Magnet-ссылка".into();
    }
    std::path::Path::new(s).file_name().map_or_else(|| s.to_string(), |n| n.to_string_lossy().into_owned())
}
