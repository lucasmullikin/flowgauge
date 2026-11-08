//! All numeric SMC sensors, plus fan state. Labels per chip come later, so keys are raw for now.

use serde::Serialize;
use smc::Smc;
use std::cell::{Cell, RefCell};

/// Each sensor read is a kernel call, and sensors are the app's biggest idle cost. So most
/// checks only read the sensors that were hottest at the last full sweep.
const FULL_SWEEP_EVERY: u32 = 20;
const HOT_CANDIDATES: usize = 24;
const HOTTEST_MIN_C: f32 = 15.0;
const HOTTEST_MAX_C: f32 = 115.0;

/// Positions of the `count` largest values, largest first.
pub fn top_positions(values: &[(usize, f32)], count: usize) -> Vec<usize> {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    sorted.into_iter().take(count).map(|(position, _)| position).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Temperature,
    Voltage,
    Current,
    Power,
}

/// SMC key prefixes. I and P on Apple Silicon are current and power, by convention, not documented.
pub fn kind_of(key: &str) -> Option<Kind> {
    match key.as_bytes().first()? {
        b'T' => Some(Kind::Temperature),
        b'V' => Some(Kind::Voltage),
        b'I' => Some(Kind::Current),
        b'P' => Some(Kind::Power),
        _ => None,
    }
}

/// Unconnected sensors read as zero or absurd values, so temperatures get a sanity range.
pub fn is_plausible(kind: Kind, value: f32) -> bool {
    value.is_finite()
        && match kind {
            Kind::Temperature => (5.0..=150.0).contains(&value),
            Kind::Voltage | Kind::Current | Kind::Power => value.abs() < 10_000.0,
        }
}

/// M1-family temperature keys with known meanings. These facts come from community research,
/// notably the Stats project. Keys change between chips, so they're only trusted on M1 Macs.
const M1_NAMES: [(&str, &str); 18] = [
    ("Tp09", "CPU efficiency core 1"),
    ("Tp0T", "CPU efficiency core 2"),
    ("Tp01", "CPU performance core 1"),
    ("Tp05", "CPU performance core 2"),
    ("Tp0D", "CPU performance core 3"),
    ("Tp0H", "CPU performance core 4"),
    ("Tp0L", "CPU performance core 5"),
    ("Tp0P", "CPU performance core 6"),
    ("Tp0X", "CPU performance core 7"),
    ("Tp0b", "CPU performance core 8"),
    ("Tg05", "GPU 1"),
    ("Tg0D", "GPU 2"),
    ("Tg0L", "GPU 3"),
    ("Tg0T", "GPU 4"),
    ("Tm02", "Memory 1"),
    ("Tm06", "Memory 2"),
    ("Tm08", "Memory 3"),
    ("Tm09", "Memory 4"),
];
/// Reported for Apple Silicon generally.
const APPLE_NAMES: [(&str, &str); 6] = [
    ("TaLP", "Airflow left"),
    ("TaRF", "Airflow right"),
    ("TH0x", "SSD"),
    ("TB1T", "Battery 1"),
    ("TB2T", "Battery 2"),
    ("TW0P", "Wi-Fi"),
];

/// A readable name. Keys with no known meaning keep their code, with only a broad area from
/// the prefix, so a guess never looks like a fact.
pub fn sensor_name(key: &str, is_m1: bool) -> String {
    let known = is_m1
        .then(|| M1_NAMES.iter().find(|(k, _)| *k == key))
        .flatten()
        .or_else(|| APPLE_NAMES.iter().find(|(k, _)| *k == key));
    if let Some((_, name)) = known {
        return (*name).to_string();
    }
    let area = match key.get(..2) {
        Some("Tp") => "CPU sensor",
        Some("Tg") => "GPU sensor",
        Some("Tm") => "Memory sensor",
        Some("TB") => "Battery sensor",
        Some("TH") => "SSD sensor",
        _ => "Sensor",
    };
    format!("{area} {key}")
}

/// Whether this Mac has an M1-family chip.
pub fn is_m1_mac() -> bool {
    let mut buffer = [0u8; 64];
    let mut size = buffer.len();
    // SAFETY: the name is NUL-terminated, and buffer has `size` writable bytes.
    let code = unsafe {
        libc::sysctlbyname(
            c"machdep.cpu.brand_string".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    code == 0 && brand_is_m1(&String::from_utf8_lossy(&buffer[..size]))
}

pub fn brand_is_m1(brand: &str) -> bool {
    brand.split_whitespace().any(|word| word == "M1")
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub key: String,
    pub kind: Kind,
    pub value: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AreaTemperature {
    pub area: &'static str,
    pub average_c: f32,
    pub peak_c: f32,
}

const AREAS: [(&str, &str); 5] = [("Tp", "CPU"), ("Tg", "GPU"), ("Tm", "Memory"), ("TH", "SSD"), ("TB", "Battery")];
const AREA_MIN_C: f32 = 15.0;

/// Average and peak per part of the Mac, going by each key's prefix. Only areas with readings
/// appear. Unconnected sensors can read near zero, so they're left out of the average.
pub fn temperature_areas(readings: &[Reading]) -> Vec<AreaTemperature> {
    AREAS
        .iter()
        .filter_map(|(prefix, area)| {
            let values: Vec<f32> = readings
                .iter()
                .filter(|r| r.kind == Kind::Temperature && r.key.starts_with(prefix) && r.value >= AREA_MIN_C)
                .map(|r| r.value)
                .collect();
            let peak_c = values.iter().copied().max_by(f32::total_cmp)?;
            Some(AreaTemperature {
                area,
                average_c: values.iter().sum::<f32>() / values.len() as f32,
                peak_c,
            })
        })
        .collect()
}

/// What fan curves follow: the warmer of the CPU and GPU averages. Single sensors spike well
/// above 90C under loads where macOS barely runs the fans, so following the hottest one would
/// hold them near full speed for no benefit. The helper's safety limit still watches every sensor.
pub fn curve_temperature(areas: &[AreaTemperature]) -> Option<f32> {
    areas
        .iter()
        .filter(|a| a.area == "CPU" || a.area == "GPU")
        .map(|a| a.average_c)
        .max_by(f32::total_cmp)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fan {
    pub index: u8,
    pub mode: u8,
    pub target_rpm: f32,
    pub actual_rpm: f32,
    pub min_rpm: f32,
    pub max_rpm: f32,
}

/// Remembers which keys are numeric so later reads skip scanning all the SMC's keys.
pub struct Sensors {
    smc: Smc,
    keys: Vec<(String, u32, Kind)>,
    candidates: RefCell<Vec<usize>>,
    checks: Cell<u32>,
}

impl Sensors {
    pub fn open() -> Result<Sensors, smc::Error> {
        let smc = Smc::open()?;
        let mut keys = Vec::new();
        for index in 0..smc.key_count()? {
            let Ok(key) = smc.key_at(index) else { continue };
            if let Some(kind) = kind_of(&key)
                && smc.read_f32(&key).is_ok()
            {
                keys.push((key.clone(), smc::fourcc(&key)?, kind));
            }
        }
        Ok(Sensors { smc, keys, candidates: RefCell::default(), checks: Cell::new(0) })
    }

    pub fn read(&self) -> Vec<Reading> {
        self.read_where(|_| true)
    }

    /// Only touches the requested kinds, since each sensor read is an IOKit call.
    pub fn read_kind(&self, wanted: Kind) -> Vec<Reading> {
        self.read_where(|kind| kind == wanted)
    }

    fn read_where(&self, wanted: impl Fn(Kind) -> bool) -> Vec<Reading> {
        self.keys
            .iter()
            .filter(|(_, _, kind)| wanted(*kind))
            .filter_map(|(key, code, kind)| {
                let value = self.smc.read_known_f32(*code).ok()?;
                is_plausible(*kind, value).then(|| Reading {
                    key: key.clone(),
                    kind: *kind,
                    value,
                })
            })
            .collect()
    }

    fn temperature_at(&self, position: usize) -> Option<f32> {
        let (_, code, kind) = self.keys.get(position)?;
        if *kind != Kind::Temperature {
            return None;
        }
        self.smc
            .read_known_f32(*code)
            .ok()
            .filter(|c| (HOTTEST_MIN_C..=HOTTEST_MAX_C).contains(c))
    }

    pub fn hottest(&self) -> Option<(String, f32)> {
        let check = self.checks.get();
        self.checks.set(check.wrapping_add(1));
        let positions: Vec<usize> = if check.is_multiple_of(FULL_SWEEP_EVERY) {
            (0..self.keys.len()).collect()
        } else {
            self.candidates.borrow().clone()
        };
        let readings: Vec<(usize, f32)> = positions
            .into_iter()
            .filter_map(|position| self.temperature_at(position).map(|c| (position, c)))
            .collect();
        if check.is_multiple_of(FULL_SWEEP_EVERY) {
            *self.candidates.borrow_mut() = top_positions(&readings, HOT_CANDIDATES);
        }
        readings
            .into_iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(position, c)| (self.keys[position].0.clone(), c))
    }

    /// The whole-Mac power readings. Missing keys read as zero, and the display key is only
    /// reported for some chips.
    pub fn power(&self) -> crate::battery::PowerSample {
        let watts = |key: &str| f64::from(self.smc.read_f32(key).unwrap_or(0.0).max(0.0));
        crate::battery::PowerSample {
            adapter_watts: watts("PDTR"),
            battery_watts: watts("PPBR"),
            system_watts: watts("PSTR"),
            display_watts: watts("PDBR"),
        }
    }

    pub fn fans(&self) -> Result<Vec<Fan>, smc::Error> {
        let mut fans = Vec::new();
        for index in 0..self.smc.read_u8("FNum")? {
            let float = |suffix: &str| self.smc.read_f32(&format!("F{index}{suffix}"));
            fans.push(Fan {
                index,
                mode: self.smc.read_u8(&format!("F{index}Md"))?,
                target_rpm: float("Tg")?,
                actual_rpm: float("Ac")?,
                min_rpm: float("Mn")?,
                max_rpm: float("Mx")?,
            });
        }
        Ok(fans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_key_prefix() {
        assert_eq!(kind_of("Tp0c"), Some(Kind::Temperature));
        assert_eq!(kind_of("VP0R"), Some(Kind::Voltage));
        assert_eq!(kind_of("IBAC"), Some(Kind::Current));
        assert_eq!(kind_of("PSTR"), Some(Kind::Power));
        assert_eq!(kind_of("F0Ac"), None);
        assert_eq!(kind_of(""), None);
    }

    #[test]
    fn rejects_implausible_values() {
        assert!(is_plausible(Kind::Temperature, 45.0));
        assert!(!is_plausible(Kind::Temperature, 1.0));
        assert!(!is_plausible(Kind::Temperature, 129.0 + 100.0));
        assert!(!is_plausible(Kind::Power, f32::NAN));
        assert!(is_plausible(Kind::Current, -2.5));
    }

    #[test]
    fn areas_summarize_by_prefix_and_skip_dead_sensors() {
        let reading = |key: &str, value| Reading { key: key.into(), kind: Kind::Temperature, value };
        let got = temperature_areas(&[
            reading("Tp01", 60.0),
            reading("Tp05", 80.0),
            reading("Tp09", 6.0),
            reading("TB1T", 31.0),
            Reading { key: "Tpxx".into(), kind: Kind::Power, value: 500.0 },
        ]);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], AreaTemperature { area: "CPU", average_c: 70.0, peak_c: 80.0 });
        assert_eq!(got[1].area, "Battery");
    }

    #[test]
    fn curves_follow_the_warmer_of_cpu_and_gpu_averages() {
        let area = |area, average_c, peak_c| AreaTemperature { area, average_c, peak_c };
        let areas = [area("CPU", 60.0, 97.0), area("GPU", 66.0, 70.0), area("Battery", 80.0, 80.0)];
        assert_eq!(curve_temperature(&areas), Some(66.0));
        assert_eq!(curve_temperature(&[area("SSD", 40.0, 40.0)]), None);
    }

    #[test]
    fn known_keys_get_names_and_unknown_ones_keep_their_code() {
        assert_eq!(sensor_name("Tp0D", true), "CPU performance core 3");
        assert_eq!(sensor_name("TW0P", true), "Wi-Fi");
        assert_eq!(sensor_name("TW0P", false), "Wi-Fi");
        assert_eq!(sensor_name("Tp02", true), "CPU sensor Tp02");
        assert_eq!(sensor_name("TCMz", true), "Sensor TCMz");
    }

    #[test]
    fn m1_names_are_not_trusted_on_other_chips() {
        assert_eq!(sensor_name("Tp0D", false), "CPU sensor Tp0D");
    }

    #[test]
    fn recognizes_m1_family_brands() {
        assert!(brand_is_m1("Apple M1 Max"));
        assert!(brand_is_m1("Apple M1"));
        assert!(!brand_is_m1("Apple M2 Max"));
        assert!(!brand_is_m1("Apple M10"));
    }

    #[test]
    fn top_positions_are_the_largest_first() {
        assert_eq!(top_positions(&[(4, 40.0), (7, 90.0), (2, 60.0)], 2), [7, 2]);
        assert!(top_positions(&[], 2).is_empty());
    }

    #[test]
    fn quick_checks_agree_with_a_full_sweep() {
        let sensors = Sensors::open().unwrap();
        let (_, full) = sensors.hottest().unwrap();
        let (_, quick) = sensors.hottest().unwrap();
        assert!((full - quick).abs() < 25.0, "{full} vs {quick}");
    }

    #[test]
    fn reads_real_sensors_and_fans() {
        let sensors = Sensors::open().unwrap();
        assert!(sensors.read().iter().any(|r| r.kind == Kind::Temperature));
        let temperatures = sensors.read_kind(Kind::Temperature);
        assert!(!temperatures.is_empty());
        assert!(temperatures.iter().all(|r| r.kind == Kind::Temperature));
        let fans = sensors.fans().unwrap();
        assert!(fans.iter().all(|fan| fan.max_rpm > fan.min_rpm));
    }
}
