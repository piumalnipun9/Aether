// SPDX-License-Identifier: GPL-3.0-or-later

//! What the compositor and the settings app share.
//!
//! The app is a separate program. It reads the shell's state and asks for changes over a
//! Unix socket, with one line of JSON each way per request. The socket is in the user's
//! runtime directory, so only they can reach it.

use std::{
    env,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// The settings app's Wayland app id, which is also its desktop entry's name.
pub const SETTINGS_APP_ID: &str = "org.aether.Settings";

/// A page of the settings app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Page {
    Network,
    Devices,
    Display,
    Style,
    Sound,
    Notifications,
    Battery,
    About,
}

impl Page {
    /// All of them, in the app's order.
    pub const ALL: [Self; 8] = [
        Self::Network,
        Self::Devices,
        Self::Display,
        Self::Style,
        Self::Sound,
        Self::Notifications,
        Self::Battery,
        Self::About,
    ];

    /// The name of this page on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Devices => "devices",
            Self::Display => "display",
            Self::Style => "style",
            Self::Sound => "sound",
            Self::Notifications => "notifications",
            Self::Battery => "battery",
            Self::About => "about",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|page| page.name() == name)
    }
}

/// An RGB color.
pub type Rgb = [u8; 3];

/// The theme's colors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colors {
    pub surface: Rgb,
    pub primary: Rgb,
    pub on_primary: Rgb,
    pub accent: Rgb,
    pub on_surface: Rgb,
    pub on_surface_variant: Rgb,
    pub error: Rgb,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Network {
    pub ssid: String,
    /// From 1 to 3.
    pub bars: u8,
    pub secured: bool,
    pub active: bool,
    pub known: bool,
    pub connecting: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BluetoothDevice {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub connected: bool,
    pub paired: bool,
    pub connecting: bool,
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sink {
    pub name: String,
    pub description: String,
    pub active: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub name: String,
    pub resolution: String,
    pub scale: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// Everything the settings app shows that the compositor knows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// A page the app was asked to open while it was running.
    pub page: Option<Page>,
    pub colors: Colors,
    pub error: Option<String>,
    pub wifi_enabled: bool,
    pub networks: Vec<Network>,
    pub bluetooth_enabled: bool,
    pub bluetooth_devices: Vec<BluetoothDevice>,
    pub brightness: Option<u8>,
    pub dark: bool,
    pub night_light: bool,
    pub night_light_available: bool,
    pub outputs: Vec<Output>,
    pub wallpaper: Option<PathBuf>,
    pub volume: Option<u8>,
    pub muted: bool,
    pub sinks: Vec<Sink>,
    pub mic_muted: bool,
    pub do_not_disturb: bool,
    pub notification_count: usize,
    pub battery: Option<Battery>,
    pub power_saver: Option<bool>,
    pub stay_awake: bool,
    pub rounded_corners: bool,
    pub blur: bool,
}

/// What the settings app can ask of the compositor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Answered with a [`Snapshot`]. Everything else is answered with `null`.
    Snapshot,
    /// The app is on this page, so keep what it shows up to date.
    Showing(Page),
    ToggleWifi,
    ClickNetwork {
        ssid: String,
    },
    ConnectWifi {
        ssid: String,
        password: String,
    },
    ForgetWifi {
        ssid: String,
    },
    ToggleBluetooth,
    ClickBluetooth {
        id: String,
    },
    SetBrightness(u8),
    ToggleDark,
    ToggleNightLight,
    SetScale {
        output: String,
        scale: f32,
    },
    SetWallpaper(PathBuf),
    SetVolume(u8),
    ToggleMute,
    SetSink(String),
    ToggleMicrophone,
    ToggleDoNotDisturb,
    ClearNotifications,
    TogglePowerSaver,
    ToggleStayAwake,
    ToggleRoundedCorners,
    ToggleBlur,
}

/// Where the compositor on the Wayland socket `display` listens.
pub fn socket_path(display: &str) -> Option<PathBuf> {
    socket_in(Path::new(&env::var_os("XDG_RUNTIME_DIR")?), display)
}

fn socket_in(runtime_dir: &Path, display: &str) -> Option<PathBuf> {
    // WAYLAND_DISPLAY can be a whole path.
    let name = Path::new(display).file_name()?.to_str()?;
    Some(runtime_dir.join(format!("aether-shell-{name}.sock")))
}

/// How well `text` matches `query`, which is expected in lowercase. Higher is better.
pub fn match_score(text: &str, query: &str) -> Option<u32> {
    let text = text.to_lowercase();

    if text == query {
        Some(400)
    } else if text.starts_with(query) {
        Some(300)
    } else if text
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(query))
    {
        Some(200)
    } else {
        text.contains(query).then_some(100)
    }
}

/// A topic in the settings app.
pub struct Topic {
    pub page: Page,
    pub title: &'static str,
    pub subtitle: &'static str,
    keywords: &'static [&'static str],
}

pub const TOPICS: [Topic; 8] = [
    Topic {
        page: Page::Network,
        title: "Network & internet",
        subtitle: "Wi-Fi, saved networks",
        keywords: &["wifi", "wireless", "internet", "password", "connection"],
    },
    Topic {
        page: Page::Devices,
        title: "Connected devices",
        subtitle: "Bluetooth, pairing",
        keywords: &[
            "bluetooth",
            "headphones",
            "earbuds",
            "mouse",
            "keyboard",
            "pair",
        ],
    },
    Topic {
        page: Page::Display,
        title: "Display",
        subtitle: "Brightness, Dark theme, scale",
        keywords: &[
            "brightness",
            "dark",
            "light",
            "night",
            "scale",
            "monitor",
            "screen",
        ],
    },
    Topic {
        page: Page::Style,
        title: "Wallpaper & style",
        subtitle: "Wallpaper, corners, blur",
        keywords: &[
            "rounded",
            "corners",
            "square",
            "blur",
            "transparency",
            "background",
            "theme",
            "colors",
            "picture",
        ],
    },
    Topic {
        page: Page::Sound,
        title: "Sound",
        subtitle: "Volume, output, microphone",
        keywords: &["audio", "volume", "speaker", "microphone", "mute"],
    },
    Topic {
        page: Page::Notifications,
        title: "Notifications",
        subtitle: "Do Not Disturb, clear all",
        keywords: &["alerts", "dnd", "disturb"],
    },
    Topic {
        page: Page::Battery,
        title: "Battery",
        subtitle: "Charge, Battery Saver",
        keywords: &["power", "saver", "charging", "stay", "awake", "idle"],
    },
    Topic {
        page: Page::About,
        title: "About",
        subtitle: "Device, software, hardware",
        keywords: &[
            "system",
            "version",
            "kernel",
            "memory",
            "processor",
            "hostname",
        ],
    },
];

/// The settings topics for `query`, which is expected in lowercase, best first.
/// All of them match nothing being asked.
pub fn matching_topics(query: &str) -> Vec<&'static Topic> {
    let mut topics = TOPICS
        .iter()
        .filter_map(|topic| {
            let title = match_score(topic.title, query);
            let subtitle = match_score(topic.subtitle, query).map(|score| score / 2);
            let keyword = topic
                .keywords
                .iter()
                .filter_map(|keyword| match_score(keyword, query))
                .max()
                .map(|score| score / 2);
            Some((title.or(subtitle).or(keyword)?, topic))
        })
        .collect::<Vec<_>>();

    topics.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    topics.into_iter().map(|(_, topic)| topic).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_go_by_their_names() {
        for page in Page::ALL {
            assert_eq!(Page::from_name(page.name()), Some(page));
        }
        assert_eq!(Page::from_name("nothing"), None);
    }

    #[test]
    fn requests_fit_on_one_line() {
        let request = Request::ConnectWifi {
            ssid: "two\nlines".into(),
            password: "a \"quoted\" one".into(),
        };
        let line = serde_json::to_string(&request).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), request);
    }

    #[test]
    fn snapshots_survive_the_trip() {
        let snapshot = Snapshot {
            page: Some(Page::Sound),
            brightness: Some(40),
            ..Default::default()
        };
        let line = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(serde_json::from_str::<Snapshot>(&line).unwrap(), snapshot);
    }

    #[test]
    fn the_socket_lives_with_the_display_name() {
        let path = socket_in(Path::new("/run/user/1"), "/some/where/wayland-1");
        assert_eq!(
            path.as_deref(),
            Some(Path::new("/run/user/1/aether-shell-wayland-1.sock"))
        );
    }
}
