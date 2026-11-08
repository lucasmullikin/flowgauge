//! Graph history. The last hour stays in memory at full detail; SQLite keeps 1-minute averages
//! and peaks for 35 days, and hourly ones forever, so the record keeps growing for as long as
//! the app is used. That costs a few MB a year. Disk writes happen once a minute, which doesn't
//! wear the SSD, at the cost of up to a minute of data on a crash. A normal quit saves everything.

use rusqlite::{Connection, params};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

pub const RECENT_SECONDS: i64 = 3600;
const MINUTE: i64 = 60;
const HOUR: i64 = 3600;
const MINUTE_RETENTION_SECONDS: i64 = 35 * 86400;
const TIER_MINUTE: i64 = 0;
const TIER_HOUR: i64 = 1;
const MINUTE_TIER_MAX_RANGE: i64 = 30 * 86400;
pub const APP_RETENTION_SECONDS: i64 = 90 * 86400;
pub const HOST_RETENTION_SECONDS: i64 = 7 * 86400;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Row {
    pub t: i64,
    pub avg: f32,
    pub max: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bucket {
    pub avg: f32,
    pub max: f32,
}

/// Groups rows into evenly spaced buckets. Empty buckets stay `None`, so a sleeping Mac shows as
/// a gap in the graph rather than a made-up line.
pub fn bucketize(rows: &[Row], start: i64, end: i64, points: usize) -> Vec<Option<Bucket>> {
    if points == 0 || end <= start {
        return Vec::new();
    }
    let width = (end - start) as f64 / points as f64;
    let mut totals = vec![(0.0f64, 0u32, f32::MIN); points];
    for row in rows {
        if row.t < start || row.t >= end {
            continue;
        }
        let index = (((row.t - start) as f64 / width) as usize).min(points - 1);
        let slot = &mut totals[index];
        slot.0 += f64::from(row.avg);
        slot.1 += 1;
        slot.2 = slot.2.max(row.max);
    }
    totals
        .into_iter()
        .map(|(sum, count, max)| {
            (count > 0).then(|| Bucket { avg: (sum / f64::from(count)) as f32, max })
        })
        .collect()
}

/// Connects neighboring readings across empty buckets. Monitors report every few seconds and
/// saved history once a minute, so at fine zoom most buckets are empty even though nothing is
/// missing. Gaps longer than `max_gap_seconds` stay gaps, so sleep still shows as a break.
pub fn bridge_gaps(buckets: &mut [Option<Bucket>], bucket_seconds: f64, max_gap_seconds: f64) {
    let mut previous: Option<usize> = None;
    for index in 0..buckets.len() {
        if buckets[index].is_none() {
            continue;
        }
        if let Some(from) = previous {
            let missing = index - from - 1;
            if missing > 0 && (index - from) as f64 * bucket_seconds <= max_gap_seconds {
                let (start, end) = (buckets[from], buckets[index]);
                if let (Some(start), Some(end)) = (start, end) {
                    for step in 1..=missing {
                        let share = step as f32 / (missing + 1) as f32;
                        buckets[from + step] = Some(Bucket {
                            avg: start.avg + (end.avg - start.avg) * share,
                            max: start.max + (end.max - start.max) * share,
                        });
                    }
                }
            }
        }
        previous = Some(index);
    }
}

#[derive(Clone, Copy)]
struct Accumulator {
    sum: f64,
    count: u32,
    max: f32,
}

pub struct History {
    connection: Connection,
    recent: HashMap<String, VecDeque<(i64, f32)>>,
    pending: BTreeMap<(String, i64), Accumulator>,
    pending_totals: BTreeMap<(String, String), (u64, u64)>,
    /// Keyed by hour and app.
    pending_apps: BTreeMap<(i64, String), (u64, u64)>,
    /// Keyed by hour, app and remote address.
    pending_hosts: BTreeMap<(i64, String, String), (u64, u64)>,
    last_retention: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppHour {
    pub hour: i64,
    pub app: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

impl History {
    pub fn open(path: &Path) -> rusqlite::Result<History> {
        if let Some(parent) = path.parent() {
            let _ = crate::settings::make_private_dir(parent);
        }
        Self::with_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> rusqlite::Result<History> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(connection: Connection) -> rusqlite::Result<History> {
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS samples(
               tier INTEGER, series TEXT, t INTEGER, avg REAL, max REAL, n INTEGER,
               PRIMARY KEY(tier, series, t)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS totals(
               day TEXT, interface TEXT, bytes_in INTEGER, bytes_out INTEGER,
               PRIMARY KEY(day, interface)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS app_hours(
               hour INTEGER, app TEXT, bytes_in INTEGER, bytes_out INTEGER,
               PRIMARY KEY(hour, app)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS host_hours(
               hour INTEGER, app TEXT, host TEXT, bytes_in INTEGER, bytes_out INTEGER,
               PRIMARY KEY(hour, app, host)) WITHOUT ROWID;",
        )?;
        Ok(History {
            connection,
            recent: HashMap::new(),
            pending: BTreeMap::new(),
            pending_totals: BTreeMap::new(),
            pending_apps: BTreeMap::new(),
            pending_hosts: BTreeMap::new(),
            last_retention: 0,
        })
    }

    pub fn record(&mut self, series: &str, t: i64, value: f32) {
        if !value.is_finite() {
            return;
        }
        let recent = self.recent.entry(series.to_string()).or_default();
        recent.push_back((t, value));
        while recent.front().is_some_and(|(first, _)| *first < t - RECENT_SECONDS) {
            recent.pop_front();
        }
        let slot = self
            .pending
            .entry((series.to_string(), t - t.rem_euclid(MINUTE)))
            .or_insert(Accumulator { sum: 0.0, count: 0, max: f32::MIN });
        slot.sum += f64::from(value);
        slot.count += 1;
        slot.max = slot.max.max(value);
    }

    /// Writes finished minutes to disk. Call about once a minute.
    pub fn flush(&mut self, now: i64) -> rusqlite::Result<()> {
        let written = self.write(now - now.rem_euclid(MINUTE));
        self.expire(now)?;
        written
    }

    /// Writes everything, including the minute in progress, for when the app is quitting.
    pub fn flush_all(&mut self, now: i64) -> rusqlite::Result<()> {
        let written = self.write(i64::MAX);
        self.expire(now)?;
        written
    }

    /// Deletes what's past its keeping time, at most hourly. Runs even when saving new data
    /// failed, so old app and destination records never outstay what Settings promises.
    fn expire(&mut self, now: i64) -> rusqlite::Result<()> {
        if now - self.last_retention < HOUR {
            return Ok(());
        }
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM samples WHERE tier = ?1 AND t < ?2",
            params![TIER_MINUTE, now - MINUTE_RETENTION_SECONDS],
        )?;
        transaction.execute("DELETE FROM app_hours WHERE hour < ?1", params![now - APP_RETENTION_SECONDS])?;
        transaction.execute("DELETE FROM host_hours WHERE hour < ?1", params![now - HOST_RETENTION_SECONDS])?;
        transaction.commit()?;
        self.last_retention = now;
        Ok(())
    }

    fn write(&mut self, before: i64) -> rusqlite::Result<()> {
        let finished: Vec<(String, i64)> =
            self.pending.keys().filter(|(_, t)| *t < before).cloned().collect();
        let transaction = self.connection.transaction()?;
        {
            let mut minute = transaction.prepare_cached(
                "INSERT OR REPLACE INTO samples VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut hour = transaction.prepare_cached(
                "INSERT INTO samples VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(tier, series, t) DO UPDATE SET
                   avg = (avg * n + excluded.avg * excluded.n) / (n + excluded.n),
                   max = MAX(max, excluded.max),
                   n = n + excluded.n",
            )?;
            let mut totals = transaction.prepare_cached(
                "INSERT INTO totals VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(day, interface) DO UPDATE SET
                   bytes_in = bytes_in + excluded.bytes_in,
                   bytes_out = bytes_out + excluded.bytes_out",
            )?;
            for ((day, interface), (down, up)) in &self.pending_totals {
                totals.execute(params![day, interface, *down as i64, *up as i64])?;
            }
            let mut apps = transaction.prepare_cached(
                "INSERT INTO app_hours VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(hour, app) DO UPDATE SET
                   bytes_in = bytes_in + excluded.bytes_in,
                   bytes_out = bytes_out + excluded.bytes_out",
            )?;
            for ((hour, app), (down, up)) in &self.pending_apps {
                apps.execute(params![hour, app, *down as i64, *up as i64])?;
            }
            let mut hosts = transaction.prepare_cached(
                "INSERT INTO host_hours VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(hour, app, host) DO UPDATE SET
                   bytes_in = bytes_in + excluded.bytes_in,
                   bytes_out = bytes_out + excluded.bytes_out",
            )?;
            for ((hour, app, host), (down, up)) in &self.pending_hosts {
                hosts.execute(params![hour, app, host, *down as i64, *up as i64])?;
            }
            for key in &finished {
                let (series, t) = key;
                let a = self.pending[key];
                let avg = a.sum / f64::from(a.count);
                minute.execute(params![TIER_MINUTE, series, t, avg, f64::from(a.max), a.count])?;
                let hour_t = t - t.rem_euclid(HOUR);
                hour.execute(params![TIER_HOUR, series, hour_t, avg, f64::from(a.max), a.count])?;
            }
        }
        transaction.commit()?;
        self.pending_totals.clear();
        self.pending_apps.clear();
        self.pending_hosts.clear();
        for key in finished {
            self.pending.remove(&key);
        }
        Ok(())
    }

    /// When recording began, across restarts. `None` until something has been recorded.
    pub fn first_time(&self) -> rusqlite::Result<Option<i64>> {
        let stored: Option<i64> =
            self.connection.query_row("SELECT MIN(t) FROM samples WHERE tier = ?1", params![TIER_HOUR], |row| row.get(0))?;
        let stored_minute: Option<i64> =
            self.connection.query_row("SELECT MIN(t) FROM samples WHERE tier = ?1", params![TIER_MINUTE], |row| row.get(0))?;
        // Hourly rows are stamped at the top of the hour, so prefer the finer minute time when
        // minutes still reach back that far.
        let saved = match (stored, stored_minute) {
            (Some(hour), Some(minute)) if minute - hour < HOUR => Some(minute),
            (hour, minute) => hour.or(minute),
        };
        let waiting = self.pending.keys().map(|(_, t)| *t).min();
        Ok(match (saved, waiting) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    }

    /// Rows for a graph window, using the finest data available for that range.
    pub fn rows(&self, series: &str, range: i64, now: i64) -> rusqlite::Result<Vec<Row>> {
        let start = now - range;
        let tier = if range <= MINUTE_TIER_MAX_RANGE { TIER_MINUTE } else { TIER_HOUR };
        let mut statement = self.connection.prepare_cached(
            "SELECT t, avg, max FROM samples WHERE tier = ?1 AND series = ?2 AND t >= ?3 ORDER BY t",
        )?;
        let stored = statement.query_map(params![tier, series, start], |row| {
            Ok(Row {
                t: row.get(0)?,
                avg: row.get::<_, f64>(1)? as f32,
                max: row.get::<_, f64>(2)? as f32,
            })
        })?;
        let mut rows = stored.collect::<rusqlite::Result<Vec<Row>>>()?;
        if range <= RECENT_SECONDS {
            let recent = self.recent.get(series);
            let first_recent = recent.and_then(|r| r.front()).map_or(i64::MAX, |(t, _)| *t);
            rows.retain(|r| r.t < first_recent);
            if let Some(recent) = recent {
                rows.extend(recent.iter().filter(|(t, _)| *t >= start).map(|(t, value)| Row {
                    t: *t,
                    avg: *value,
                    max: *value,
                }));
            }
        } else {
            let last_stored = rows.last().map_or(i64::MIN, |r| r.t);
            for ((name, t), a) in self.pending.range((series.to_string(), start)..) {
                if name != series {
                    break;
                }
                if *t > last_stored {
                    rows.push(Row {
                        t: *t,
                        avg: (a.sum / f64::from(a.count)) as f32,
                        max: a.max,
                    });
                }
            }
        }
        Ok(rows)
    }

    /// Watt-hours used since `start`, from a series of watts. Saved minutes count for 60 seconds
    /// each, and time with no readings counts as nothing.
    pub fn energy_wh(&self, series: &str, start: i64) -> rusqlite::Result<f64> {
        let saved: f64 = self.connection.query_row(
            "SELECT COALESCE(SUM(avg), 0) FROM samples WHERE tier = ?1 AND series = ?2 AND t >= ?3",
            params![TIER_MINUTE, series, start],
            |row| row.get(0),
        )?;
        let waiting: f64 = self
            .pending
            .range((series.to_string(), start)..)
            .take_while(|((name, _), _)| name == series)
            .map(|(_, a)| a.sum / f64::from(a.count))
            .sum();
        Ok((saved + waiting) * MINUTE as f64 / HOUR as f64)
    }

    /// Adds traffic to a day's total; it reaches disk at the next flush.
    pub fn add_total(&mut self, day: &str, interface: &str, bytes_in: u64, bytes_out: u64) {
        let entry = self.pending_totals.entry((day.to_string(), interface.to_string())).or_default();
        entry.0 += bytes_in;
        entry.1 += bytes_out;
    }

    /// Adds one app's traffic to the hour containing `t`.
    pub fn add_app_usage(&mut self, t: i64, app: &str, bytes_in: u64, bytes_out: u64) {
        let entry = self.pending_apps.entry((t - t.rem_euclid(HOUR), app.to_string())).or_default();
        entry.0 += bytes_in;
        entry.1 += bytes_out;
    }

    /// Adds traffic between one app and one remote address to the hour containing `t`.
    pub fn add_host_usage(&mut self, t: i64, app: &str, host: &str, bytes_in: u64, bytes_out: u64) {
        let entry = self.pending_hosts.entry((t - t.rem_euclid(HOUR), app.to_string(), host.to_string())).or_default();
        entry.0 += bytes_in;
        entry.1 += bytes_out;
    }

    /// Bytes per app in cells of `cell` seconds from `start`, summed in the database so even 90
    /// days stays a short query. Cell `i` covers `start + i * cell`.
    pub fn app_cells(&self, start: i64, cell: i64) -> rusqlite::Result<Vec<(i64, String, u64)>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT (hour - ?1) / ?2, app, SUM(bytes_in + bytes_out) FROM app_hours
             WHERE hour >= ?1 GROUP BY (hour - ?1) / ?2, app",
        )?;
        let rows = statement.query_map(params![start, cell.max(1)], |row| {
            Ok(((row.get::<_, i64>(0)?, row.get::<_, String>(1)?), row.get::<_, i64>(2)? as u64))
        })?;
        let mut cells: HashMap<(i64, String), u64> = rows.collect::<rusqlite::Result<_>>()?;
        for ((hour, app), (down, up)) in self.pending_apps.range((start, String::new())..) {
            *cells.entry(((hour - start) / cell.max(1), app.clone())).or_default() += down + up;
        }
        Ok(cells.into_iter().map(|((index, app), bytes)| (index, app, bytes)).collect())
    }

    /// Total traffic per app since `start`, including what hasn't been saved yet.
    pub fn app_totals(&self, start: i64) -> rusqlite::Result<Vec<AppHour>> {
        let start_hour = start - start.rem_euclid(HOUR);
        let mut statement = self.connection.prepare_cached(
            "SELECT app, SUM(bytes_in), SUM(bytes_out) FROM app_hours WHERE hour >= ?1 GROUP BY app",
        )?;
        let rows = statement.query_map(params![start_hour], |row| {
            Ok((row.get::<_, String>(0)?, (row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)? as u64)))
        })?;
        let mut totals: HashMap<String, (u64, u64)> = rows.collect::<rusqlite::Result<_>>()?;
        for ((_, app), (down, up)) in self.pending_apps.range((start_hour, String::new())..) {
            let entry = totals.entry(app.clone()).or_default();
            entry.0 += down;
            entry.1 += up;
        }
        Ok(totals
            .into_iter()
            .map(|(app, (bytes_in, bytes_out))| AppHour { hour: start_hour, app, bytes_in, bytes_out })
            .collect())
    }

    /// Traffic per app and remote address since `start`, including what hasn't been saved yet.
    pub fn host_totals(&self, start: i64) -> rusqlite::Result<BTreeMap<(String, String), (u64, u64)>> {
        let start_hour = start - start.rem_euclid(HOUR);
        let mut statement = self.connection.prepare_cached(
            "SELECT app, host, SUM(bytes_in), SUM(bytes_out) FROM host_hours WHERE hour >= ?1 GROUP BY app, host",
        )?;
        let rows = statement.query_map(params![start_hour], |row| {
            Ok(((row.get(0)?, row.get(1)?), (row.get::<_, i64>(2)? as u64, row.get::<_, i64>(3)? as u64)))
        })?;
        let mut all: BTreeMap<(String, String), (u64, u64)> = rows.collect::<rusqlite::Result<_>>()?;
        for ((hour, app, host), (down, up)) in &self.pending_hosts {
            if *hour >= start_hour {
                let entry = all.entry((app.clone(), host.clone())).or_default();
                entry.0 += down;
                entry.1 += up;
            }
        }
        Ok(all)
    }

    /// Forgets which apps used the network and where they connected. Speeds and daily totals stay.
    pub fn clear_network(&mut self) -> rusqlite::Result<()> {
        self.pending_apps.clear();
        self.pending_hosts.clear();
        self.connection.execute_batch("DELETE FROM app_hours; DELETE FROM host_hours;")
    }

    /// Sum across all interfaces for days at or after `from_day` (YYYY-MM-DD sorts correctly).
    pub fn totals_since(&self, from_day: &str) -> rusqlite::Result<(u64, u64)> {
        self.connection.query_row(
            "SELECT COALESCE(SUM(bytes_in), 0), COALESCE(SUM(bytes_out), 0) FROM totals WHERE day >= ?1",
            params![from_day],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )
    }
}

/// Local date as YYYY-MM-DD, `days_ago` days back.
pub fn local_day(now: i64, days_ago: i64) -> String {
    let moment: libc::time_t = now - days_ago * 86400;
    // SAFETY: all-zero is valid for tm, and localtime_r only writes through the given pointers.
    let parts = unsafe {
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&moment, &mut parts);
        parts
    };
    format!("{:04}-{:02}-{:02}", parts.tm_year + 1900, parts.tm_mon + 1, parts.tm_mday)
}

/// Local time like "Nov 5, 13:07", for showing when history began.
pub fn format_time(t: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let moment: libc::time_t = t;
    // SAFETY: all-zero is valid for tm, and localtime_r only writes through the given pointers.
    let parts = unsafe {
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&moment, &mut parts);
        parts
    };
    let month = MONTHS.get(parts.tm_mon as usize).copied().unwrap_or("?");
    format!("{month} {}, {:02}:{:02}", parts.tm_mday, parts.tm_hour, parts.tm_min)
}

/// When today began, in local time.
pub fn local_midnight(now: i64) -> i64 {
    let moment: libc::time_t = now;
    // SAFETY: all-zero is valid for tm, and these calls only use the pointers they're given.
    unsafe {
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&moment, &mut parts);
        parts.tm_hour = 0;
        parts.tm_min = 0;
        parts.tm_sec = 0;
        parts.tm_isdst = -1;
        libc::mktime(&mut parts)
    }
}

pub fn month_start_day(now: i64) -> String {
    let today = local_day(now, 0);
    format!("{}-01", &today[..7])
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_800_000_000;

    #[test]
    fn bucketize_averages_and_keeps_peaks() {
        let rows = [
            Row { t: 0, avg: 10.0, max: 10.0 },
            Row { t: 1, avg: 30.0, max: 50.0 },
            Row { t: 5, avg: 7.0, max: 7.0 },
        ];
        let got = bucketize(&rows, 0, 10, 2);
        assert_eq!(got[0], Some(Bucket { avg: (10.0 + 30.0 + 0.0) / 2.0, max: 50.0 }));
        assert_eq!(got[1], Some(Bucket { avg: 7.0, max: 7.0 }));
    }

    #[test]
    fn empty_time_stays_a_gap() {
        let rows = [Row { t: 9, avg: 1.0, max: 1.0 }];
        let got = bucketize(&rows, 0, 10, 10);
        assert_eq!(got[0], None);
        assert!(got[9].is_some());
    }

    #[test]
    fn bucketize_handles_degenerate_input() {
        assert!(bucketize(&[], 0, 10, 0).is_empty());
        assert!(bucketize(&[], 10, 10, 5).is_empty());
    }

    #[test]
    fn short_gaps_are_connected_and_long_ones_stay_broken() {
        let reading = |avg| Some(Bucket { avg, max: avg });
        let mut short = [reading(10.0), None, None, reading(40.0)];
        bridge_gaps(&mut short, 24.0, 180.0);
        assert_eq!(short[1], reading(20.0));
        assert_eq!(short[2], reading(30.0));
        let mut long = [reading(10.0), None, None, reading(40.0)];
        bridge_gaps(&mut long, 600.0, 180.0);
        assert_eq!(long[1], None);
    }

    #[test]
    fn nothing_is_invented_before_or_after_the_data() {
        let reading = Some(Bucket { avg: 5.0, max: 5.0 });
        let mut buckets = [None, reading, None];
        bridge_gaps(&mut buckets, 1.0, 180.0);
        assert_eq!(buckets, [None, reading, None]);
    }

    #[test]
    fn short_ranges_read_second_by_second_from_memory() {
        let mut history = History::open_in_memory().unwrap();
        for second in 0..30 {
            history.record("x", T + second, second as f32);
        }
        let rows = history.rows("x", 600, T + 30).unwrap();
        assert_eq!(rows.len(), 30);
        assert_eq!(rows[29].avg, 29.0);
    }

    #[test]
    fn flush_stores_finished_minutes_only_with_avg_and_peak() {
        let mut history = History::open_in_memory().unwrap();
        history.record("x", T, 10.0);
        history.record("x", T + 30, 30.0);
        history.record("x", T + 61, 99.0);
        history.flush(T + 62).unwrap();
        assert!(history.pending.contains_key(&("x".to_string(), T + 60)));
        assert!(!history.pending.contains_key(&("x".to_string(), T)));
        let rows = history.rows("x", 86400, T + 62).unwrap();
        assert_eq!(rows[0], Row { t: T, avg: 20.0, max: 30.0 });
        assert_eq!(rows[1].t, T + 60);
    }

    #[test]
    fn long_ranges_use_hourly_rollups_weighted_by_samples() {
        let mut history = History::open_in_memory().unwrap();
        history.record("x", T, 10.0);
        history.record("x", T + 1, 10.0);
        history.record("x", T + 60, 40.0);
        history.flush(T + 120).unwrap();
        let rows = history.rows("x", 31536000, T + 120).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].avg, 20.0);
        assert_eq!(rows[0].max, 40.0);
    }

    #[test]
    fn data_survives_a_reopen() {
        let path = std::env::temp_dir().join(format!("gauge-history-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let mut history = History::open(&path).unwrap();
            history.record("x", T, 5.0);
            history.flush(T + 120).unwrap();
        }
        let history = History::open(&path).unwrap();
        assert_eq!(history.rows("x", 86400, T + 120).unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hourly_history_is_never_deleted() {
        let mut history = History::open_in_memory().unwrap();
        history.record("x", T, 5.0);
        let decade_later = T + 3650 * 86400;
        history.flush(decade_later).unwrap();
        history.last_retention = 0;
        history.flush(decade_later).unwrap();
        assert_eq!(history.rows("x", 3651 * 86400, decade_later).unwrap().len(), 1);
    }

    #[test]
    fn quitting_saves_the_minute_in_progress() {
        let path = std::env::temp_dir().join(format!("gauge-quit-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let mut history = History::open(&path).unwrap();
            history.record("x", T + 5, 7.0);
            history.add_total("2026-11-05", "en0", 100, 10);
            history.flush_all(T + 6).unwrap();
        }
        let history = History::open(&path).unwrap();
        assert_eq!(history.rows("x", 86400, T + 6).unwrap().len(), 1);
        assert_eq!(history.totals_since("2026-11-05").unwrap(), (100, 10));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn first_time_spans_saved_and_waiting_data() {
        let mut history = History::open_in_memory().unwrap();
        assert_eq!(history.first_time().unwrap(), None);
        history.record("x", T + 125, 1.0);
        assert_eq!(history.first_time().unwrap(), Some(T + 120));
        history.flush(T + 300).unwrap();
        assert_eq!(history.first_time().unwrap(), Some(T + 120));
        history.record("x", T + 400, 1.0);
        assert_eq!(history.first_time().unwrap(), Some(T + 120));
    }

    #[test]
    fn old_minutes_are_deleted_but_hours_are_kept() {
        let mut history = History::open_in_memory().unwrap();
        history.record("x", T, 5.0);
        let later = T + 40 * 86400;
        history.flush(later).unwrap();
        history.last_retention = 0;
        history.flush(later).unwrap();
        assert!(history.rows("x", 45 * 86400, later).unwrap().len() == 1);
        let minutes: i64 = history
            .connection
            .query_row("SELECT COUNT(*) FROM samples WHERE tier = 0", [], |r| r.get(0))
            .unwrap();
        assert_eq!(minutes, 0);
    }

    #[test]
    fn non_finite_values_are_ignored() {
        let mut history = History::open_in_memory().unwrap();
        history.record("x", T, f32::NAN);
        assert!(history.rows("x", 600, T + 1).unwrap().is_empty());
    }

    #[test]
    fn totals_accumulate_by_day() {
        let mut history = History::open_in_memory().unwrap();
        history.add_total("2026-11-01", "en0", 100, 10);
        history.add_total("2026-11-01", "en0", 50, 5);
        history.add_total("2026-11-05", "utun1", 1, 1);
        history.flush(T).unwrap();
        history.add_total("2026-11-05", "utun1", 0, 0);
        history.flush(T).unwrap();
        assert_eq!(history.totals_since("2026-11-01").unwrap(), (151, 16));
        assert_eq!(history.totals_since("2026-11-05").unwrap(), (1, 1));
    }

    #[test]
    fn energy_adds_up_minutes_of_power() {
        let mut history = History::open_in_memory().unwrap();
        for minute in 0..60 {
            history.record("w", T + minute * 60, 100.0);
        }
        assert!((history.energy_wh("w", T).unwrap() - 100.0).abs() < 0.01);
        history.flush(T + 1800).unwrap();
        assert!((history.energy_wh("w", T).unwrap() - 100.0).abs() < 0.01, "saved and waiting both count");
        assert!((history.energy_wh("w", T + 1800).unwrap() - 50.0).abs() < 0.01);
        assert_eq!(history.energy_wh("none", T).unwrap(), 0.0);
    }

    #[test]
    fn midnight_is_today_and_within_a_day() {
        let midnight = local_midnight(T);
        assert!(midnight <= T && T - midnight < 90_000);
        assert_eq!(local_day(midnight, 0), local_day(T, 0));
    }

    #[test]
    fn local_days_are_formatted_for_sorting() {
        assert_eq!(local_day(T, 0).len(), 10);
        assert!(local_day(T, 1) < local_day(T, 0));
        assert!(month_start_day(T).ends_with("-01"));
        assert!(format_time(T).contains(", "));
    }

    #[test]
    fn app_and_host_usage_add_up_before_and_after_saving() {
        let mut history = History::open_in_memory().unwrap();
        let hour = T - T.rem_euclid(HOUR);
        history.add_app_usage(hour + 10, "Mail", 100, 10);
        history.add_host_usage(hour + 10, "Mail", "203.0.113.5", 100, 10);
        history.flush_all(T).unwrap();
        history.add_app_usage(hour + 20, "Mail", 50, 5);
        history.add_app_usage(hour + HOUR, "Safari", 7, 0);
        history.add_host_usage(hour + 20, "Mail", "203.0.113.5", 50, 5);
        let totals = history.app_totals(hour).unwrap();
        let mail = totals.iter().find(|r| r.app == "Mail").unwrap();
        assert_eq!((mail.bytes_in, mail.bytes_out), (150, 15));
        let mut cells = history.app_cells(hour, HOUR).unwrap();
        cells.sort();
        assert_eq!(cells, vec![(0, "Mail".to_string(), 165), (1, "Safari".to_string(), 7)]);
        let hosts = history.host_totals(hour).unwrap();
        assert_eq!(hosts[&("Mail".to_string(), "203.0.113.5".to_string())], (150, 15));
        history.flush_all(T).unwrap();
        assert_eq!(history.app_totals(hour).unwrap().iter().find(|r| r.app == "Mail").unwrap().bytes_in, 150);
    }

    #[test]
    fn network_history_expires_and_can_be_cleared() {
        let mut history = History::open_in_memory().unwrap();
        history.add_app_usage(T - APP_RETENTION_SECONDS - 2 * HOUR, "Old", 1, 1);
        history.add_host_usage(T - HOST_RETENTION_SECONDS - 2 * HOUR, "Old", "192.0.2.1", 1, 1);
        history.add_app_usage(T, "New", 1, 1);
        history.flush_all(T).unwrap();
        let apps = history.app_totals(0).unwrap();
        assert!(apps.iter().all(|r| r.app != "Old"), "{apps:?}");
        assert!(history.host_totals(0).unwrap().is_empty());
        history.add_total(&local_day(T, 0), "en0", 5, 5);
        history.clear_network().unwrap();
        assert!(history.app_totals(0).unwrap().is_empty());
        history.flush_all(T).unwrap();
        assert_eq!(history.totals_since(&local_day(T, 0)).unwrap(), (5, 5), "daily totals stay");
    }
}
