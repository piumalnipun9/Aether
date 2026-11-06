// SPDX-License-Identifier: GPL-3.0-or-later

//! System status shown in the bar, read from `/proc`, `/sys`, BlueZ and `pactl`.

use std::{fs, process::Command};

/// Wi-Fi's link quality is reported out of this.
const MAX_LINK_QUALITY: f64 = 70.0;

/// The most bars a Wi-Fi signal can have.
const MAX_WIFI_BARS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Status {
    pub wifi: Radio,
    /// The signal's strength from 1 to [`MAX_WIFI_BARS`], when connected.
    pub wifi_bars: u8,
    pub bluetooth: Radio,
    pub sound: Option<Sound>,
    pub battery: Option<Battery>,
    /// CPU usage in percent.
    pub cpu: u8,
    /// Memory usage in percent.
    pub ram: u8,
}

/// The state of a wireless radio.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Radio {
    /// There's no such hardware.
    #[default]
    Missing,
    Off,
    On,
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sound {
    pub percent: u8,
    pub muted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Battery {
    pub percent: u8,
    /// Whether it's plugged in, even if it isn't taking any charge at the moment.
    pub charging: bool,
}

/// CPU time counters, in ticks since boot.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

/// Parses the aggregate `cpu` line of `/proc/stat`.
fn parse_cpu_times(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().next()?.strip_prefix("cpu ")?;
    let fields = line
        .split_whitespace()
        .map(|field| field.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;

    Some(CpuTimes {
        // Guest time is already counted in user time, so only take the first 8 fields.
        total: fields.iter().take(8).sum(),
        idle: fields.get(3)? + fields.get(4)?,
    })
}

/// CPU usage between two samples.
fn cpu_usage(old: CpuTimes, new: CpuTimes) -> u8 {
    let total = new.total.saturating_sub(old.total);
    if total == 0 {
        return 0;
    }
    let busy = total.saturating_sub(new.idle.saturating_sub(old.idle));
    (busy * 100 / total) as u8
}

/// Parses memory usage from `/proc/meminfo`.
fn parse_ram_usage(meminfo: &str) -> Option<u8> {
    let field = |name: &str| -> Option<u64> {
        meminfo
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };

    let total = field("MemTotal").filter(|total| *total > 0)?;
    let available = field("MemAvailable")?;
    Some((total.saturating_sub(available) * 100 / total) as u8)
}

/// Parses `interface`'s signal strength from `/proc/net/wireless`.
fn parse_wifi_strength(wireless: &str, interface: &str) -> Option<u8> {
    let line = wireless
        .lines()
        .find(|line| line.trim_start().starts_with(&format!("{interface}:")))?;
    let quality = line
        .split_whitespace()
        .nth(2)?
        .trim_end_matches('.')
        .parse::<f64>()
        .ok()?;
    Some((quality / MAX_LINK_QUALITY * 100.0).clamp(0.0, 100.0) as u8)
}

/// How many bars a signal of `strength` percent gets.
pub fn wifi_bars(strength: u8) -> u8 {
    (strength as u16 * MAX_WIFI_BARS as u16 / 100 + 1).min(MAX_WIFI_BARS as u16) as u8
}

/// Whether any radio of `kind` is switched off through rfkill.
pub fn is_blocked(kind: &str) -> bool {
    let Ok(entries) = fs::read_dir("/sys/class/rfkill") else {
        return false;
    };

    entries.flatten().any(|entry| {
        let read = |name: &str| fs::read_to_string(entry.path().join(name)).unwrap_or_default();
        read("type").trim() == kind && (read("soft").trim() == "1" || read("hard").trim() == "1")
    })
}

/// Reads the state of the Wi-Fi radio and the strength of its connection.
fn read_wifi() -> (Radio, Option<u8>) {
    let interfaces = fs::read_dir("/sys/class/net")
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().join("wireless").exists())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if interfaces.is_empty() {
        return (Radio::Missing, None);
    }
    if is_blocked("wlan") {
        return (Radio::Off, None);
    }

    let wireless = fs::read_to_string("/proc/net/wireless").unwrap_or_default();
    let strength = interfaces.iter().find_map(|entry| {
        let operstate = fs::read_to_string(entry.path().join("operstate")).ok()?;
        let interface = entry.file_name();
        (operstate.trim() == "up").then(|| parse_wifi_strength(&wireless, interface.to_str()?))?
    });

    match strength {
        Some(strength) => (Radio::Connected, Some(strength)),
        None => (Radio::On, None),
    }
}

/// Works out Bluetooth's state from the names in `/sys/class/bluetooth`,
/// where `hci0` is an adapter and `hci0:1` is a connection of it.
fn bluetooth_state<'a>(
    mut names: impl Iterator<Item = &'a str>,
    blocked: bool,
    is_powered: impl FnOnce(&str) -> bool,
) -> Radio {
    let (adapters, connections) = names
        .by_ref()
        .partition::<Vec<_>, _>(|name| !name.contains(':'));

    match adapters.first() {
        None => Radio::Missing,
        Some(adapter) if blocked || !is_powered(adapter) => Radio::Off,
        Some(_) if connections.is_empty() => Radio::On,
        Some(_) => Radio::Connected,
    }
}

/// Asks BlueZ whether `adapter` is powered, which rfkill doesn't know about.
/// Without an answer, it's assumed to be.
fn is_powered(bus: Option<&zbus::blocking::Connection>, adapter: &str) -> bool {
    let powered = || -> zbus::Result<bool> {
        let reply = bus.ok_or(zbus::Error::Unsupported)?.call_method(
            Some("org.bluez"),
            format!("/org/bluez/{adapter}"),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.bluez.Adapter1", "Powered"),
        )?;
        Ok(bool::try_from(
            reply.body().deserialize::<zbus::zvariant::Value<'_>>()?,
        )?)
    };

    powered().unwrap_or(true)
}

fn read_bluetooth(bus: Option<&zbus::blocking::Connection>) -> Radio {
    let names = fs::read_dir("/sys/class/bluetooth")
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    bluetooth_state(
        names.iter().map(String::as_str),
        is_blocked("bluetooth"),
        |adapter| is_powered(bus, adapter),
    )
}

/// Parses the first percentage from `pactl get-sink-volume`.
fn parse_volume(output: &str) -> Option<u8> {
    let (before, _) = output.split_once('%')?;
    let digits = before.rsplit(|c: char| !c.is_ascii_digit()).next()?;
    // Volumes can go past 100%.
    Some(digits.parse::<u32>().ok()?.min(u8::MAX as u32) as u8)
}

fn read_sound() -> Option<Sound> {
    let pactl = |command: &str| -> Option<String> {
        let output = Command::new("pactl")
            .args([command, "@DEFAULT_SINK@"])
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };

    Some(Sound {
        percent: parse_volume(&pactl("get-sink-volume")?)?,
        muted: pactl("get-sink-mute")?.trim() == "Mute: yes",
    })
}

fn read_battery() -> Option<Battery> {
    fs::read_dir("/sys/class/power_supply")
        .ok()?
        .flatten()
        .find_map(|entry| {
            let path = entry.path();
            let read = |name: &str| fs::read_to_string(path.join(name));
            // Wireless mice and the like report their batteries here too, as devices.
            let is_systems = read("scope").map_or(true, |scope| scope.trim() != "Device");
            if read("type").ok()?.trim() != "Battery" || !is_systems {
                return None;
            }
            Some(Battery {
                percent: read("capacity").ok()?.trim().parse().ok()?,
                charging: read("status").ok()?.trim() != "Discharging",
            })
        })
}

/// Reads the status. CPU usage is what happened between two reads.
pub struct Reader {
    bus: Option<zbus::blocking::Connection>,
    last_cpu: CpuTimes,
}

fn read_cpu() -> Option<CpuTimes> {
    parse_cpu_times(&fs::read_to_string("/proc/stat").ok()?)
}

impl Reader {
    pub fn new(bus: Option<zbus::blocking::Connection>) -> Self {
        Self {
            bus,
            last_cpu: read_cpu().unwrap_or_default(),
        }
    }

    pub fn read(&mut self) -> Status {
        let cpu = read_cpu().unwrap_or(self.last_cpu);
        let ram = fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|meminfo| parse_ram_usage(&meminfo));
        let (wifi, wifi_strength) = read_wifi();

        let status = Status {
            wifi,
            wifi_bars: wifi_strength.map_or(0, wifi_bars),
            bluetooth: read_bluetooth(self.bus.as_ref()),
            sound: read_sound(),
            battery: read_battery(),
            cpu: cpu_usage(self.last_cpu, cpu),
            ram: ram.unwrap_or_default(),
        };
        self.last_cpu = cpu;

        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu() {
        let old = parse_cpu_times("cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 1 2").unwrap();
        let new = parse_cpu_times("cpu  150 0 150 900 0 0 0 0 0 0").unwrap();
        assert_eq!(old.total, 1000);
        assert_eq!(cpu_usage(old, new), 50);
        assert_eq!(cpu_usage(new, new), 0);
    }

    #[test]
    fn iowait_counts_as_idle() {
        let old = parse_cpu_times("cpu  0 0 0 0 0 0 0 0").unwrap();
        let new = parse_cpu_times("cpu  25 0 0 25 50 0 0 0").unwrap();
        assert_eq!(cpu_usage(old, new), 25);
    }

    #[test]
    fn ram() {
        let meminfo = "MemTotal:  1000 kB\nMemFree: 1 kB\nMemAvailable:  250 kB\n";
        assert_eq!(parse_ram_usage(meminfo), Some(75));
        assert_eq!(parse_ram_usage("MemTotal: 0 kB\nMemAvailable: 0 kB"), None);
    }

    #[test]
    fn wifi() {
        let wireless = concat!(
            "Inter-| sta-|   Quality        |\n",
            " face | tus | link level noise |\n",
            "  wlo1: 0000   35.  -256.  -256        0      0      0      3    362        0\n",
        );
        assert_eq!(parse_wifi_strength(wireless, "wlo1"), Some(50));
        assert_eq!(parse_wifi_strength(wireless, "wlan0"), None);
    }

    #[test]
    fn bars() {
        assert_eq!(wifi_bars(0), 1);
        assert_eq!(wifi_bars(33), 1);
        assert_eq!(wifi_bars(34), 2);
        assert_eq!(wifi_bars(57), 2);
        assert_eq!(wifi_bars(67), 3);
        assert_eq!(wifi_bars(100), 3);
    }

    #[test]
    fn bluetooth() {
        let state = |names: &[&'static str], blocked, powered| {
            bluetooth_state(names.iter().copied(), blocked, |_| powered)
        };

        assert_eq!(state(&[], false, true), Radio::Missing);
        assert_eq!(state(&["hci0"], true, true), Radio::Off);
        assert_eq!(state(&["hci0"], false, false), Radio::Off);
        assert_eq!(state(&["hci0"], false, true), Radio::On);
        assert_eq!(state(&["hci0", "hci0:1"], false, true), Radio::Connected);
        assert_eq!(state(&["hci0:1", "hci0"], true, true), Radio::Off);
    }

    #[test]
    fn volume() {
        let output =
            "Volume: front-left: 33026 /  50% / -17.86 dB,   front-right: 33026 /  51% /\n";
        assert_eq!(parse_volume(output), Some(50));
        assert_eq!(
            parse_volume("Volume: mono: 98304 / 150% / 10 dB"),
            Some(150)
        );
        assert_eq!(parse_volume("Failed to get sink"), None);
    }
}
