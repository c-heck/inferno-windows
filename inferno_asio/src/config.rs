use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub asio_frames: i32,
    pub sample_rate: u32,
    pub network_ms: u32,
    pub channels: usize,
    pub bind_ip: Option<Ipv4Addr>,
    pub loopback: bool,
    pub power_plan: bool,
    pub device_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default = "default_asio_frames")]
    pub asio_frames: i32,
    pub sample_rate: u32,
    pub network_ms: u32,
    /// None selects the maximum channel count for the current sample rate.
    pub channels: Option<usize>,
    pub bind_ip: Option<Ipv4Addr>,
    pub loopback: bool,
    /// Switch to the low-latency power plan while streaming.
    #[serde(default = "default_power_plan")]
    pub power_plan: bool,
    /// Dante device name; None uses the generated default.
    #[serde(default)]
    pub device_name: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            asio_frames: default_asio_frames(),
            sample_rate: 48000,
            network_ms: 20,
            channels: None,
            bind_ip: None,
            loopback: false,
            power_plan: default_power_plan(),
            device_name: None,
        }
    }
}

/// Dante device names: 1-31 of A-Z, a-z, 0-9 and '-', not starting or ending with '-'.
pub fn valid_device_name(name: &str) -> bool {
    (1..=31).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Nearest supported network buffer for a latency requested by Dante Controller.
pub fn snap_network_ms(latency_ns: u32) -> u32 {
    let ms = latency_ns as f64 / 1_000_000.0;
    *NETWORK_MS
        .iter()
        .min_by(|a, b| {
            (**a as f64 - ms)
                .abs()
                .total_cmp(&(**b as f64 - ms).abs())
        })
        .unwrap()
}

pub const NETWORK_MS: [u32; 4] = [4, 6, 10, 20];

pub fn settings_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("INFERNO_ASIO_CONFIG") {
        if path.is_empty() {
            return Err("INFERNO_ASIO_CONFIG must not be empty".into());
        }
        return Ok(PathBuf::from(path));
    }
    let base = env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?;
    Ok(PathBuf::from(base)
        .join("InfernoArmAsio")
        .join("settings.json"))
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, String> {
        match fs::read(path) {
            Ok(data) => {
                let settings: Self = serde_json::from_slice(&data)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                settings.resolve()?;
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("Read {}: {e}", path.display())),
        }
    }

    pub fn resolve(&self) -> Result<Config, String> {
        let config = Config {
            asio_frames: self.asio_frames,
            sample_rate: self.sample_rate,
            network_ms: self.network_ms,
            channels: self.channels.unwrap_or(max_channels(self.sample_rate)?),
            bind_ip: self.bind_ip,
            loopback: self.loopback,
            power_plan: self.power_plan,
            device_name: self.device_name.clone(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        use std::{io::Write, os::windows::ffi::OsStrExt};
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        self.resolve()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or("Settings path must have a parent directory")?;
        fs::create_dir_all(parent).map_err(|e| format!("Create settings directory: {e}"))?;
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| format!("Create {}: {e}", temporary.display()))?;
        let result = (|| {
            file.write_all(
                &serde_json::to_vec_pretty(self).map_err(|e| format!("Encode settings: {e}"))?,
            )
            .map_err(|e| format!("Write settings: {e}"))?;
            file.sync_all()
                .map_err(|e| format!("Flush settings: {e}"))?;
            drop(file);
            let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let dest: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe {
                MoveFileExW(
                    source.as_ptr(),
                    dest.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(format!(
                    "Replace settings: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        })();
        if result.is_err() && temporary.exists() {
            if let Err(e) = fs::remove_file(&temporary) {
                return Err(format!(
                    "{result:?}; temporary settings cleanup failed: {e}"
                ));
            }
        }
        result
    }
}

/// Applies a change to the saved settings file (used for Dante Controller edits).
pub fn update_saved(change: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let path = settings_path()?;
    let mut settings = Settings::load(&path)?;
    change(&mut settings);
    settings.save(&path)
}

pub fn load_configuration() -> Result<(Config, usize), String> {
    let saved = Settings::load(&settings_path()?)?;
    resolve_overrides(saved, |name| match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(format!("{name}: {e}")),
    })
}

fn resolve_overrides(
    mut settings: Settings,
    get: impl Fn(&str) -> Result<Option<String>, String>,
) -> Result<(Config, usize), String> {
    if let Some(value) = get("INFERNO_ASIO_RATE")? {
        settings.sample_rate = parse_number("INFERNO_ASIO_RATE", &value)?;
    }
    if let Some(value) = get("INFERNO_ASIO_CHANNELS")? {
        settings.channels = Some(parse_number("INFERNO_ASIO_CHANNELS", &value)? as usize);
    }
    if let Some(value) = get("INFERNO_ASIO_NETWORK_MS")? {
        settings.network_ms = parse_number("INFERNO_ASIO_NETWORK_MS", &value)?;
    }
    if let Some(value) = get("INFERNO_ASIO_BIND_IP")? {
        settings.bind_ip = Some(value.parse().map_err(|_| "Invalid INFERNO_ASIO_BIND_IP")?);
    }
    if let Some(value) = get("INFERNO_ASIO_MODE")? {
        settings.loopback = match value.as_str() {
            "network" => false,
            "loopback" => true,
            _ => return Err("INFERNO_ASIO_MODE must be network or loopback".into()),
        };
    }
    let limit = settings.channels.unwrap_or(64);
    Ok((settings.resolve()?, limit))
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        load_configuration().map(|(config, _)| config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !valid_buffer(self.asio_frames) {
            return Err("ASIO buffer must be 64, 128, 256, 512, 1024, or 2048 samples".into());
        }
        if self.channels == 0 || self.channels > max_channels(self.sample_rate)? {
            return Err(format!(
                "Channel count exceeds the {} Hz profile",
                self.sample_rate
            ));
        }
        if ![20, 10, 6, 4].contains(&self.network_ms) {
            return Err("Network buffer must be 20, 10, 6, or 4 ms".into());
        }
        if let Some(ip) = self.bind_ip {
            if ip.is_unspecified() || ip.is_multicast() || ip.is_loopback() || ip.is_broadcast() {
                return Err("BIND_IP must identify a local wired Ethernet IPv4 address".into());
            }
        }
        if let Some(name) = &self.device_name {
            if !valid_device_name(name) {
                return Err("Dante name: 1-31 letters, digits or '-', not starting/ending with '-'".into());
            }
        }
        Ok(())
    }

    pub fn network_frames(&self) -> usize {
        (self.sample_rate as usize * self.network_ms as usize).div_ceil(1000)
    }
}

fn parse_number(name: &str, value: &str) -> Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

pub fn max_channels(rate: u32) -> Result<usize, String> {
    match rate {
        44100 | 48000 => Ok(64),
        96000 => Ok(32),
        _ => Err("Supported sample rates: 44100, 48000, 96000 Hz".into()),
    }
}

pub fn valid_buffer(frames: i32) -> bool {
    (64..=2048).contains(&frames) && (frames as u32).is_power_of_two()
}

pub const ASIO_BUFFERS: [i32; 6] = [64, 128, 256, 512, 1024, 2048];

fn default_asio_frames() -> i32 {
    512
}

fn default_power_plan() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_requested_profiles_and_network_buffers() {
        for rate in [44100, 48000, 96000] {
            for ms in [20, 10, 6, 4] {
                let config = Config {
                    asio_frames: 256,
                    sample_rate: rate,
                    channels: max_channels(rate).unwrap(),
                    network_ms: ms,
                    bind_ip: None,
                    loopback: true,
                    power_plan: true,
                    device_name: None,
                };
                assert!(config.validate().is_ok());
                assert!(config.network_frames() * 1000 >= rate as usize * ms as usize);
                assert!(config.network_frames() * 1000 < rate as usize * ms as usize + 1000);
            }
        }
        assert_eq!(max_channels(44100), Ok(64));
        assert_eq!(max_channels(48000), Ok(64));
        assert_eq!(max_channels(96000), Ok(32));
        assert!(max_channels(88200).is_err());
    }
    #[test]
    fn rejects_invalid_profiles() {
        let mut c = Config {
            asio_frames: 256,
            sample_rate: 96000,
            channels: 64,
            network_ms: 20,
            bind_ip: None,
            loopback: false,
            power_plan: true,
            device_name: None,
        };
        assert!(c.validate().is_err());
        c.channels = 32;
        c.network_ms = 5;
        assert!(c.validate().is_err());
        c.network_ms = 4;
        c.channels = 0;
        assert!(c.validate().is_err());
        for n in [-1, 0, 32, 65, 4096] {
            assert!(!valid_buffer(n));
        }
        for n in [64, 128, 256, 512, 1024, 2048] {
            assert!(valid_buffer(n));
        }
    }
    #[test]
    fn saved_settings_and_environment_precedence() {
        let saved = Settings {
            asio_frames: 512,
            sample_rate: 48000,
            channels: Some(2),
            network_ms: 10,
            bind_ip: Some(Ipv4Addr::new(192, 168, 1, 2)),
            loopback: false,
            power_plan: false,
            device_name: Some("Studio-PC".into()),
        };
        let (config, limit) = resolve_overrides(saved.clone(), |_| Ok(None)).unwrap();
        assert_eq!((config.channels, config.network_ms, limit), (2, 10, 2));
        assert_eq!(config.asio_frames, 512);
        assert!(!config.power_plan);
        assert_eq!(config.device_name.as_deref(), Some("Studio-PC"));
        let (config, _) = resolve_overrides(saved, |key| {
            Ok(match key {
                "INFERNO_ASIO_RATE" => Some("96000".into()),
                "INFERNO_ASIO_NETWORK_MS" => Some("4".into()),
                "INFERNO_ASIO_MODE" => Some("loopback".into()),
                _ => None,
            })
        })
        .unwrap();
        assert_eq!(
            (config.sample_rate, config.network_ms, config.channels),
            (96000, 4, 2)
        );
        assert!(config.loopback);
        assert!(resolve_overrides(Settings::default(), |_| Ok(Some("bad".into()))).is_err());
    }
    #[test]
    fn asio_buffer_settings_validate_and_preserve_legacy_files() {
        let legacy = r#"{"sample_rate":48000,"network_ms":20,"channels":null,"bind_ip":null,"loopback":false}"#;
        let mut settings: Settings = serde_json::from_str(legacy).unwrap();
        assert!(settings.power_plan);
        assert_eq!(settings.device_name, None);
        assert_eq!(Settings::default().resolve().unwrap().asio_frames, 512);
        assert_eq!(settings.resolve().unwrap().asio_frames, 512);
        for frames in ASIO_BUFFERS {
            settings.asio_frames = frames;
            let encoded = serde_json::to_vec(&settings).unwrap();
            let decoded: Settings = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded.resolve().unwrap().asio_frames, frames);
        }
        for frames in [-1, 0, 65, 4096] {
            settings.asio_frames = frames;
            assert!(settings.resolve().is_err());
        }
    }
    #[test]
    fn controller_names_and_latencies_are_normalized() {
        for name in ["Studio-PC", "A", "abc123", &"x".repeat(31)] {
            assert!(valid_device_name(name), "{name}");
        }
        for name in ["", "-a", "a-", "has space", "ümlaut", &"x".repeat(32)] {
            assert!(!valid_device_name(name), "{name}");
        }
        for (ns, ms) in [(150_000, 4), (1_000_000, 4), (5_000_000, 4), (5_100_000, 6),
            (6_000_000, 6), (10_000_000, 10), (15_000_000, 10), (16_000_000, 20), (40_000_000, 20)]
        {
            assert_eq!(snap_network_ms(ns), ms, "{ns}");
        }
    }
    #[test]
    fn atomic_settings_roundtrip_and_corruption() {
        let directory =
            env::temp_dir().join(format!("inferno-settings-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("settings.json");
        let mut settings = Settings::default();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        settings.save(&path).unwrap();
        settings.network_ms = 6;
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temporary, b"owned by another save").unwrap();
        assert!(settings.save(&path).is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"owned by another save");
        assert_eq!(Settings::load(&path).unwrap(), settings);
        fs::remove_file(temporary).unwrap();
        settings.network_ms = 9;
        assert!(settings.save(&path).is_err());
        assert_eq!(Settings::load(&path).unwrap().network_ms, 6);
        fs::write(&path, b"not-json").unwrap();
        assert!(Settings::load(&path).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
