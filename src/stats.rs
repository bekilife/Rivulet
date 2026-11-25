//! How much was downloaded and uploaded, by day and by torrent.
//!
//! The engine only reports running totals for the current session, so every
//! refresh adds the growth since the previous one to today's bucket. Data
//! that is merely re-checked on start (a restored download) is not counted:
//! growth only counts while a torrent stays live between two refreshes.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use rivulet::engine::{Snapshot, Status};

use crate::lang::{en, tt};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Day {
    /// Bytes downloaded and uploaded that day.
    d: u64,
    u: u64,
    /// Downloads that finished that day.
    done: u32,
    /// Torrents that uploaded something that day.
    seeded: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Torrent {
    name: String,
    size: u64,
    up: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Data {
    /// Day number (days since 1970-01-01, local time) counting started.
    since: i32,
    days: BTreeMap<i32, Day>,
    torrents: HashMap<String, Torrent>,
}

pub struct Stats {
    data: Data,
    path: PathBuf,
    /// Downloaded, uploaded and status per torrent at the previous refresh.
    last: HashMap<String, (u64, u64, Status)>,
    dirty: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Week,
    Month,
    All,
}

/// Everything the statistics screen shows for one period.
pub struct PeriodView {
    pub down: u64,
    pub up: u64,
    pub done: u32,
    pub seeded: usize,
    /// (downloaded, uploaded, tooltip label) per bar.
    pub bars: Vec<(u64, u64, String)>,
    pub axis: [String; 3],
    pub title: &'static str,
    pub range: String,
}

pub struct TopEntry {
    pub name: String,
    pub up: u64,
    pub ratio: f64,
}

fn live(s: Status) -> bool {
    matches!(s, Status::Downloading | Status::Seeding)
}

impl Stats {
    pub fn load(data_dir: &Path) -> Stats {
        let path = data_dir.join("stats.json");
        let mut data: Data =
            std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if data.since == 0 {
            data.since = today();
        }
        Stats { data, path, last: HashMap::new(), dirty: true }
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        if let Ok(b) = serde_json::to_vec(&self.data) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, b).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
                self.dirty = false;
            }
        }
    }

    pub fn reset(&mut self) {
        self.data = Data { since: today(), ..Default::default() };
        self.dirty = true;
        self.save();
    }

    /// Adds what changed since the previous call.
    pub fn record(&mut self, snaps: &[Snapshot]) {
        let day_key = today();
        for s in snaps {
            let cur = (s.downloaded, s.uploaded, s.status);
            // Count growth only from a live state (not right after a
            // re-check on start) to anything but a new check.
            if let Some(&(pd, pu, ps)) = self.last.get(&s.info_hash)
                && live(ps)
                && s.status != Status::Checking
            {
                let dd = cur.0.saturating_sub(pd);
                let du = cur.1.saturating_sub(pu);
                let finished = ps == Status::Downloading && matches!(s.status, Status::Seeding | Status::Finished);
                if dd > 0 || du > 0 || finished {
                    let day = self.data.days.entry(day_key).or_default();
                    day.d += dd;
                    day.u += du;
                    if finished {
                        day.done += 1;
                    }
                    if du > 0 && !day.seeded.contains(&s.info_hash) {
                        day.seeded.push(s.info_hash.clone());
                    }
                    self.dirty = true;
                }
                if du > 0 || !self.data.torrents.contains_key(&s.info_hash) {
                    let t = self.data.torrents.entry(s.info_hash.clone()).or_insert(Torrent {
                        name: String::new(),
                        size: 0,
                        up: 0,
                    });
                    t.up += du;
                    t.name = s.alias.clone().unwrap_or_else(|| s.name.clone());
                    t.size = s.size;
                    self.dirty = true;
                }
            }
            self.last.insert(s.info_hash.clone(), cur);
        }
    }

    /// Bytes uploaded for a torrent since counting started.
    pub fn uploaded(&self, hash: &str) -> Option<u64> {
        self.data.torrents.get(hash).map(|t| t.up)
    }

    pub fn since_label(&self) -> String {
        let (y, m, d) = civil(self.data.since);
        if en() {
            format!("Counting since {} {d}, {y}", MONTHS_EN[m as usize - 1])
        } else {
            format!("Считается с {d} {} {y}", MONTHS[m as usize - 1])
        }
    }

    /// Downloaded and uploaded per day for the last `n` days, oldest first.
    pub fn recent(&self, n: i32) -> Vec<(u64, u64)> {
        let t = today();
        (t - n + 1..=t).map(|k| self.data.days.get(&k).map_or((0, 0), |d| (d.d, d.u))).collect()
    }

    pub fn period(&self, p: Period) -> PeriodView {
        let t = today();
        let start = match p {
            Period::Week => t - 6,
            Period::Month => t - 29,
            Period::All => self.data.since.min(t),
        };
        let span = t - start + 1;
        let bucket = if span <= 31 {
            1
        } else if span <= 7 * 40 {
            7
        } else {
            30
        };
        let mut view = PeriodView {
            down: 0,
            up: 0,
            done: 0,
            seeded: 0,
            bars: Vec::new(),
            axis: Default::default(),
            title: match bucket {
                1 => tt("По дням", "By day"),
                7 => tt("По неделям", "By week"),
                _ => tt("По месяцам", "By month"),
            },
            range: match p {
                Period::All => self.since_label(),
                _ => range_label(start, t),
            },
        };
        let mut seeded: Vec<&String> = Vec::new();
        let mut k = start;
        while k <= t {
            let end = (k + bucket - 1).min(t);
            let (mut d, mut u) = (0, 0);
            for (_, day) in self.data.days.range(k..=end) {
                d += day.d;
                u += day.u;
                view.done += day.done;
                for h in &day.seeded {
                    if !seeded.contains(&h) {
                        seeded.push(h);
                    }
                }
            }
            view.down += d;
            view.up += u;
            let label = if bucket == 1 { day_label(k) } else { range_label(k, end) };
            view.bars.push((d, u, label));
            k = end + 1;
        }
        view.seeded = seeded.len();
        let n = view.bars.len() as i32;
        let starts = [start, start + (n - 1) / 2 * bucket, start + (n - 1) * bucket];
        view.axis = starts.map(short_label);
        view
    }

    /// Torrents that uploaded the most since counting started.
    pub fn top(&self, n: usize) -> Vec<TopEntry> {
        let mut v: Vec<TopEntry> = self
            .data
            .torrents
            .values()
            .filter(|t| t.up > 0)
            .map(|t| TopEntry {
                name: t.name.clone(),
                up: t.up,
                ratio: if t.size > 0 { t.up as f64 / t.size as f64 } else { 0.0 },
            })
            .collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.up));
        v.truncate(n);
        v
    }
}

// ------------------------------------------------------------ dates

const MONTHS: [&str; 12] = [
    "января",
    "февраля",
    "марта",
    "апреля",
    "мая",
    "июня",
    "июля",
    "августа",
    "сентября",
    "октября",
    "ноября",
    "декабря",
];
const MONTHS_SHORT: [&str; 12] =
    ["янв", "февр", "мар", "апр", "мая", "июн", "июл", "авг", "сент", "окт", "нояб", "дек"];

const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_SHORT_EN: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Today's local date as a day number.
pub fn today() -> i32 {
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    days_from_civil(st.wYear as i32, st.wMonth as u32, st.wDay as u32)
}

// Howard Hinnant's civil-date algorithms.
fn days_from_civil(y: i32, m: u32, d: u32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i32 - 719468
}

fn civil(z: i32) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe as i32 + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

fn day_label(k: i32) -> String {
    let (_, m, d) = civil(k);
    if en() { format!("{} {d}", MONTHS_EN[m as usize - 1]) } else { format!("{d} {}", MONTHS[m as usize - 1]) }
}

fn short_label(k: i32) -> String {
    let (_, m, d) = civil(k);
    if en() {
        format!("{} {d}", MONTHS_SHORT_EN[m as usize - 1])
    } else {
        format!("{d} {}", MONTHS_SHORT[m as usize - 1])
    }
}

/// "18–24 ноября 2026" or "26 октября — 24 ноября 2026";
/// "November 18–24, 2026" or "October 26 – November 24, 2026".
fn range_label(a: i32, b: i32) -> String {
    let (ya, ma, da) = civil(a);
    let (yb, mb, db) = civil(b);
    if en() {
        let (na, nb) = (MONTHS_EN[ma as usize - 1], MONTHS_EN[mb as usize - 1]);
        return if a == b {
            format!("{na} {da}, {ya}")
        } else if ya == yb && ma == mb {
            format!("{nb} {da}–{db}, {yb}")
        } else if ya == yb {
            format!("{na} {da} – {nb} {db}, {yb}")
        } else {
            format!("{na} {da}, {ya} – {nb} {db}, {yb}")
        };
    }
    if a == b {
        format!("{da} {} {ya}", MONTHS[ma as usize - 1])
    } else if ya == yb && ma == mb {
        format!("{da}–{db} {} {yb}", MONTHS[mb as usize - 1])
    } else if ya == yb {
        format!("{da} {} — {db} {} {yb}", MONTHS[ma as usize - 1], MONTHS[mb as usize - 1])
    } else {
        format!("{da} {} {ya} — {db} {} {yb}", MONTHS[ma as usize - 1], MONTHS[mb as usize - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        crate::lang::set(crate::lang::Lang::Ru);
        for z in [-1000, 0, 1, 20_000, 20_781, 30_000] {
            let (y, m, d) = civil(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(days_from_civil(2026, 11, 24), 20_781);
        assert_eq!(range_label(days_from_civil(2026, 11, 18), days_from_civil(2026, 11, 24)), "18–24 ноября 2026");
    }
}
