// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn parse_ids(s: &str) -> Vec<i64> {
    s.split(',')
        .filter_map(|p| p.trim().parse::<i64>().ok())
        .collect()
}

/// `YYYY-MM-DD` -> Unix-Zeit (Mitternacht UTC)
pub fn date_to_ts(date: &str) -> Option<i64> {
    let mut it = date.trim().splitn(3, '-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.get(..2)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Algorithmus "days from civil" (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400)
}

pub fn valid_date(date: &str) -> bool {
    date.len() == 10 && date_to_ts(date).is_some()
}

/// Titel aus Dateiname ableiten: Endung entfernen, Unterstriche zu Leerzeichen.
pub fn title_from_filename(name: &str) -> String {
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    let t = stem.replace(['_'], " ");
    let t = t.trim();
    if t.is_empty() { name.to_string() } else { t.to_string() }
}

pub fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dates() {
        assert_eq!(date_to_ts("1970-01-01"), Some(0));
        assert_eq!(date_to_ts("2024-03-01"), Some(1709251200));
        assert!(date_to_ts("2024-13-01").is_none());
    }
    #[test]
    fn titles() {
        assert_eq!(title_from_filename("Rechnung_2024.pdf"), "Rechnung 2024");
        assert_eq!(extension_of("a.Tar.GZ"), "gz");
        assert_eq!(extension_of("README"), "");
    }
}
