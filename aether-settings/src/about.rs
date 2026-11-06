// SPDX-License-Identifier: GPL-3.0-or-later

//! What the settings app says about the system.

use std::fs;

/// Finds the value of `KEY=value` or `KEY="value"` in `/etc/os-release`.
fn os_release_field<'a>(contents: &'a str, key: &str) -> Option<&'a str> {
    contents.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix('=')?;
        Some(value.trim_matches('"'))
    })
}

fn cpu_model(cpuinfo: &str) -> Option<&str> {
    cpuinfo.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "model name").then(|| value.trim())
    })
}

/// How much memory `/proc/meminfo` says there is, rounded to what would be on the box.
fn memory_size(meminfo: &str) -> Option<String> {
    let kilobytes = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()?;
    Some(format!("{:.0} GB", (kilobytes / 1024.0 / 1024.0).ceil()))
}

fn trimmed(path: &str) -> Option<String> {
    Some(fs::read_to_string(path).ok()?.trim().to_owned())
}

/// The system's details as pairs of a name and a value.
pub fn system_info() -> Vec<(String, String)> {
    let os = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();

    [
        ("Device name", trimmed("/proc/sys/kernel/hostname")),
        (
            "Operating system",
            os_release_field(&os, "PRETTY_NAME").map(str::to_owned),
        ),
        ("Kernel", trimmed("/proc/sys/kernel/osrelease")),
        ("Processor", cpu_model(&cpuinfo).map(str::to_owned)),
        ("Memory", memory_size(&meminfo)),
        ("Pinnacle", Some(env!("CARGO_PKG_VERSION").to_owned())),
    ]
    .into_iter()
    .filter_map(|(name, value)| Some((name.to_owned(), value?)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release() {
        let contents = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 22.04.5 LTS\"\nID=ubuntu\n";
        assert_eq!(
            os_release_field(contents, "PRETTY_NAME"),
            Some("Ubuntu 22.04.5 LTS")
        );
        assert_eq!(os_release_field(contents, "ID"), Some("ubuntu"));
        assert_eq!(os_release_field(contents, "VERSION"), None);
    }

    #[test]
    fn hardware() {
        assert_eq!(
            cpu_model("processor : 0\nmodel name\t: Intel(R) Core(TM) i5\n"),
            Some("Intel(R) Core(TM) i5")
        );
        assert_eq!(
            memory_size("MemTotal:  15740000 kB\n").as_deref(),
            Some("16 GB")
        );
    }
}
