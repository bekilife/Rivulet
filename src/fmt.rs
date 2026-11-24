//! Russian number, size, speed and time formatting.

const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];

fn scaled(bytes: u64) -> (f64, usize) {
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    (v, u)
}

fn num(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}").replace('.', ",")
}

/// "350 МБ", "1,4 ГБ". `precise` keeps one more decimal.
pub fn size(bytes: u64, precise: bool) -> String {
    let (v, u) = scaled(bytes);
    let d = match (u, precise) {
        (0, _) => 0,
        (_, false) if v >= 10.0 => 0,
        (_, false) => 1,
        (_, true) if v >= 100.0 => 1,
        (_, true) => 2,
    };
    format!("{} {}", num(v, d), UNITS[u])
}

pub fn speed(bps: u64, precise: bool) -> String {
    format!("{}/с", size(bps, precise))
}

pub fn eta(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} с"),
        60..3600 => format!("{} мин", secs.div_ceil(60)),
        3600..86400 => {
            let m = secs.div_ceil(60);
            if m.is_multiple_of(60) { format!("{} ч", m / 60) } else { format!("{} ч {} мин", m / 60, m % 60) }
        }
        _ => format!("{} дн", secs / 86400),
    }
}

pub fn percent(p: f64, precise: bool) -> String {
    let v = (p * 100.0).clamp(0.0, 100.0);
    if precise { format!("{}%", num(v, 1)) } else { format!("{}%", v.floor() as u32) }
}

/// "1 файл", "2 файла", "5 файлов".
pub fn files(n: usize) -> String {
    let word = match (n % 10, n % 100) {
        (1, r) if r != 11 => "файл",
        (2..=4, r) if !(12..=14).contains(&r) => "файла",
        _ => "файлов",
    };
    format!("{n} {word}")
}
