//! Which country an address is in, from DB-IP's free country file (CC BY 4.0,
//! https://db-ip.com). The file is downloaded once when you turn locations on, and lookups
//! happen on this Mac, so no address is ever sent anywhere.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const ATTRIBUTION: &str = "IP Geolocation by DB-IP";
const MIN_ROWS: usize = 100_000;
const FILE_NAME: &str = "dbip-country-lite.csv";

/// Address ranges, sorted by start, with a two-letter country code each.
#[derive(Debug, Default)]
pub struct Geo {
    v4: Vec<(u32, u32, [u8; 2])>,
    v6: Vec<(u128, u128, [u8; 2])>,
}

impl Geo {
    /// Parses `start,end,CC` rows. Rows that don't fit are skipped.
    pub fn parse(csv: &str) -> Geo {
        let mut geo = Geo::default();
        for line in csv.lines() {
            let mut fields = line.split(',');
            let (Some(start), Some(end), Some(code)) = (fields.next(), fields.next(), fields.next()) else { continue };
            let code = code.trim().trim_matches('"').as_bytes();
            let [a, b] = code else { continue };
            let code = [*a, *b];
            match (start.trim().parse::<IpAddr>(), end.trim().parse::<IpAddr>()) {
                (Ok(IpAddr::V4(s)), Ok(IpAddr::V4(e))) => geo.v4.push((u32::from(s), u32::from(e), code)),
                (Ok(IpAddr::V6(s)), Ok(IpAddr::V6(e))) => geo.v6.push((u128::from(s), u128::from(e), code)),
                _ => {}
            }
        }
        geo.v4.sort_unstable_by_key(|r| r.0);
        geo.v6.sort_unstable_by_key(|r| r.0);
        geo
    }

    pub fn len(&self) -> usize {
        self.v4.len() + self.v6.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The two-letter country code, or `None` for unknown and private addresses.
    pub fn country(&self, address: IpAddr) -> Option<String> {
        let address = match address {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
            v4 => v4,
        };
        let code = match address {
            IpAddr::V4(v4) => find(&self.v4, u32::from(v4)),
            IpAddr::V6(v6) => find(&self.v6, u128::from(v6)),
        }?;
        let code = std::str::from_utf8(&code).ok()?;
        (code != "ZZ").then(|| code.to_string())
    }
}

fn find<T: Ord + Copy>(ranges: &[(T, T, [u8; 2])], value: T) -> Option<[u8; 2]> {
    let index = ranges.partition_point(|r| r.0 <= value).checked_sub(1)?;
    let (_, end, code) = ranges[index];
    (value <= end).then_some(code)
}

pub fn file_path(support: &Path) -> PathBuf {
    support.join(FILE_NAME)
}

pub fn load(support: &Path) -> Option<Geo> {
    let text = std::fs::read_to_string(file_path(support)).ok()?;
    let geo = Geo::parse(&text);
    (geo.len() >= MIN_ROWS).then_some(geo)
}

/// The month's file name, like `2026-11`, from a time in seconds.
fn month(now: i64, months_back: i64) -> String {
    let moment: libc::time_t = now;
    // SAFETY: all-zero is valid for tm, and gmtime_r only writes through the given pointers.
    let parts = unsafe {
        let mut parts: libc::tm = std::mem::zeroed();
        libc::gmtime_r(&moment, &mut parts);
        parts
    };
    let total = i64::from(parts.tm_year + 1900) * 12 + i64::from(parts.tm_mon) - months_back;
    format!("{}-{:02}", total.div_euclid(12), total.rem_euclid(12) + 1)
}

/// Downloads this month's file, or last month's early in a month, and checks it before
/// replacing the old one. Contacts only download.db-ip.com.
pub fn download(support: &Path, now: i64) -> Result<usize, String> {
    crate::settings::make_private_dir(support).map_err(|e| e.to_string())?;
    let compressed = support.join(format!("{FILE_NAME}.gz.part"));
    let unpacked = support.join(format!("{FILE_NAME}.part"));
    let mut last_error = String::from("no file found");
    for back in 0..2 {
        let url = format!("https://download.db-ip.com/free/dbip-country-lite-{}.csv.gz", month(now, back));
        let status = Command::new("/usr/bin/curl")
            .args(["-fsSL", "--max-time", "120", "--proto", "=https", "-o"])
            .arg(&compressed)
            .arg(&url)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            last_error = format!("couldn't download {url}");
            continue;
        }
        let output = std::fs::File::create(&unpacked).map_err(|e| e.to_string())?;
        let unzipped = Command::new("/usr/bin/gunzip").arg("-c").arg(&compressed).stdout(output).status().map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&compressed);
        if !unzipped.success() {
            last_error = "the download wasn't a valid file".into();
            continue;
        }
        let text = std::fs::read_to_string(&unpacked).map_err(|e| e.to_string())?;
        let rows = Geo::parse(&text).len();
        if rows < MIN_ROWS {
            let _ = std::fs::remove_file(&unpacked);
            return Err(format!("the download had only {rows} rows"));
        }
        std::fs::rename(&unpacked, file_path(support)).map_err(|e| e.to_string())?;
        return Ok(rows);
    }
    let _ = std::fs::remove_file(&compressed);
    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "1.0.0.0,1.0.0.255,AU\n\
        1.0.1.0,1.0.3.255,CN\n\
        192.0.2.0,192.0.2.255,ZZ\n\
        203.0.113.0,203.0.113.255,DE\n\
        2001:200::,2001:200:ffff:ffff:ffff:ffff:ffff:ffff,JP\n\
        not,a,row\n";

    #[test]
    fn finds_the_range_an_address_falls_in() {
        let geo = Geo::parse(SAMPLE);
        assert_eq!(geo.len(), 5);
        assert_eq!(geo.country("1.0.2.9".parse().unwrap()).as_deref(), Some("CN"));
        assert_eq!(geo.country("203.0.113.200".parse().unwrap()).as_deref(), Some("DE"));
        assert_eq!(geo.country("::ffff:203.0.113.5".parse().unwrap()).as_deref(), Some("DE"));
        assert_eq!(geo.country("2001:200::1".parse().unwrap()).as_deref(), Some("JP"));
    }

    #[test]
    fn gaps_and_unknown_ranges_have_no_country() {
        let geo = Geo::parse(SAMPLE);
        assert_eq!(geo.country("1.0.4.1".parse().unwrap()), None);
        assert_eq!(geo.country("0.0.0.1".parse().unwrap()), None);
        assert_eq!(geo.country("192.0.2.1".parse().unwrap()), None);
        assert_eq!(geo.country("2001:db8::1".parse().unwrap()), None);
    }

    #[test]
    fn months_count_back_across_a_year() {
        let january_2027 = 1_798_761_600 + 86400 * 5;
        assert_eq!(month(january_2027, 0), "2027-01");
        assert_eq!(month(january_2027, 1), "2026-12");
    }

    /// Downloads the real file into a temporary folder. Run with `--ignored`.
    #[test]
    #[ignore]
    fn downloads_and_reads_the_real_file() {
        let folder = std::env::temp_dir().join(format!("gauge-geo-{}", std::process::id()));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let rows = download(&folder, now).unwrap();
        let geo = load(&folder).unwrap();
        eprintln!("rows: {rows}, 1.1.1.1 is in {:?}", geo.country("1.1.1.1".parse().unwrap()));
        assert!(geo.country("8.8.8.8".parse().unwrap()).is_some());
        let _ = std::fs::remove_dir_all(folder);
    }
}
