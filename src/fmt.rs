//! Number, size, speed and time formatting in the interface language.

use crate::lang::en;

const UNITS_RU: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
const UNITS_EN: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

pub fn unit(i: usize) -> &'static str {
    if en() { UNITS_EN[i] } else { UNITS_RU[i] }
}

fn scaled(bytes: u64) -> (f64, usize) {
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS_EN.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    (v, u)
}

/// A number with a decimal comma in Russian, a point in English.
pub fn num(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if en() { s } else { s.replace('.', ",") }
}

/// "350 MB", "1.4 GB". `precise` keeps one more decimal.
pub fn size(bytes: u64, precise: bool) -> String {
    let (v, u) = scaled(bytes);
    let d = match (u, precise) {
        (0, _) => 0,
        (_, false) if v >= 10.0 => 0,
        (_, false) => 1,
        (_, true) if v >= 100.0 => 1,
        (_, true) => 2,
    };
    format!("{} {}", num(v, d), unit(u))
}

pub fn speed(bps: u64, precise: bool) -> String {
    if en() { format!("{}/s", size(bps, precise)) } else { format!("{}/с", size(bps, precise)) }
}

pub fn eta(secs: u64) -> String {
    let (s, m, h, d) = if en() { ("s", "min", "h", "d") } else { ("с", "мин", "ч", "дн") };
    match secs {
        0..60 => format!("{secs} {s}"),
        60..3600 => format!("{} {m}", secs.div_ceil(60)),
        3600..86400 => {
            let mins = secs.div_ceil(60);
            if mins.is_multiple_of(60) {
                format!("{} {h}", mins / 60)
            } else {
                format!("{} {h} {} {m}", mins / 60, mins % 60)
            }
        }
        _ => format!("{} {d}", secs / 86400),
    }
}

pub fn percent(p: f64, precise: bool) -> String {
    let v = (p * 100.0).clamp(0.0, 100.0);
    if precise { format!("{}%", num(v, 1)) } else { format!("{}%", v.floor() as u32) }
}

/// "1 файл / 1 file", "5 файлов / 5 files".
pub fn files(n: usize) -> String {
    count(n, ["файл", "файла", "файлов"], ["file", "files"])
}

/// A count with the right word form: Russian has three, English two.
pub fn count(n: usize, ru: [&str; 3], en_forms: [&str; 2]) -> String {
    let word = if en() {
        if n == 1 { en_forms[0] } else { en_forms[1] }
    } else {
        match (n % 10, n % 100) {
            (1, r) if r != 11 => ru[0],
            (2..=4, r) if !(12..=14).contains(&r) => ru[1],
            _ => ru[2],
        }
    };
    format!("{n} {word}")
}

/// Upload ratio: "0.44", "7.3", "26".
pub fn ratio(r: f64) -> String {
    if r >= 10.0 {
        format!("{}", r.round() as u64)
    } else if r >= 1.0 {
        num(r, 1)
    } else {
        num(r, 2)
    }
}
