// SPDX-License-Identifier: GPL-3.0-or-later

//! What quick settings controls: Wi-Fi and Bluetooth through NetworkManager and BlueZ,
//! brightness through logind, sound through `pactl`, and a few smaller things.
//!
//! All of this can block, so it happens on a thread of its own that takes [`Command`]s,
//! and that also reads the bar's [`Status`].

use image::{ImageReader, RgbaImage};

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command as Process,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use tracing::warn;
use zbus::{
    blocking::{Connection, Proxy, proxy::Builder},
    proxy::CacheProperties,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

use super::{
    notifications::{self, CloseReason},
    status::{self, Status},
};

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const NM_WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const NM_AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const NM_SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
const NM_CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const NM_DEVICE_TYPE_WIFI: u32 = 2;
const NM_STATE_DISCONNECTED: u32 = 30;
const NM_STATE_PREPARE: u32 = 40;
const NM_STATE_ACTIVATED: u32 = 100;
const NM_STATE_FAILED: u32 = 120;

/// `NM80211ApSecurityFlags`.
const AP_KEY_MGMT_PSK: u32 = 0x100;
const AP_KEY_MGMT_802_1X: u32 = 0x200;
const AP_KEY_MGMT_SAE: u32 = 0x400;

const BLUEZ: &str = "org.bluez";
const BLUEZ_ADAPTER: &str = "org.bluez.Adapter1";
const BLUEZ_DEVICE: &str = "org.bluez.Device1";
const BLUEZ_BATTERY: &str = "org.bluez.Battery1";
const BLUEZ_AGENT_PATH: &str = "/org/pinnacle/agent";

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// The size album art gets scaled down to, in pixels. It's shown at about half of that.
const ART_SIZE: u32 = 160;

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";

const POWER_PROFILES: &str = "net.hadess.PowerProfiles";
const POWER_PROFILES_PATH: &str = "/net/hadess/PowerProfiles";

/// `struct rfkill_event`'s values for changing all radios of a type.
const RFKILL_TYPE_BLUETOOTH: u8 = 2;
const RFKILL_OP_CHANGE_ALL: u8 = 3;

/// How often the worker wakes up with nothing to do.
const IDLE_INTERVAL: Duration = Duration::from_secs(2);
/// The shortest time between reads of the bar's status.
const STATUS_INTERVAL: Duration = Duration::from_millis(1700);
/// How often quick settings is brought up to date while it's open.
const OPEN_INTERVAL: Duration = Duration::from_secs(1);
/// How soon to look again after doing something, as its effect takes a moment.
const AFTER_COMMAND: Duration = Duration::from_millis(300);
/// How often to look for Wi-Fi networks while they're being shown.
const SCAN_INTERVAL: Duration = Duration::from_secs(10);
/// How long a new Wi-Fi connection gets to start connecting.
const CONNECT_GRACE: Duration = Duration::from_secs(3);

/// A Wi-Fi network in range.
#[derive(Debug, Clone, PartialEq)]
pub struct Network {
    pub ssid: String,
    pub strength: u8,
    pub secured: bool,
    pub active: bool,
    pub known: bool,
    pub connecting: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BluetoothDevice {
    /// Its object path in BlueZ.
    pub id: String,
    pub name: String,
    /// A freedesktop icon name, such as `audio-headphones`.
    pub icon: String,
    pub connected: bool,
    pub paired: bool,
    pub connecting: bool,
    pub battery: Option<u8>,
}

/// An audio output.
#[derive(Debug, Clone, PartialEq)]
pub struct Sink {
    pub name: String,
    pub description: String,
    pub active: bool,
}

/// What a media player is playing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Media {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// Where it is in the track, from 0 to 1, if it's known.
    pub progress: Option<f32>,
    pub art_url: String,
    /// Only local files can be shown.
    pub art: Option<Arc<RgbaImage>>,
}

/// The state the shell's panels show.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct System {
    pub wifi_enabled: bool,
    /// The network it's connected to.
    pub wifi_name: Option<String>,
    pub networks: Vec<Network>,
    pub bluetooth_enabled: bool,
    /// A device it's connected to.
    pub bluetooth_name: Option<String>,
    pub bluetooth_devices: Vec<BluetoothDevice>,
    pub sinks: Vec<Sink>,
    /// In percent, if there's a backlight to control.
    pub brightness: Option<u8>,
    pub mic_muted: bool,
    /// `None` without a power profile daemon.
    pub power_saver: Option<bool>,
    pub dark: bool,
    pub media: Option<Media>,
    pub error: Option<String>,
}

/// Which details are worth keeping up to date.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Watching {
    /// Quick settings is closed.
    #[default]
    Nothing,
    Tiles,
    Wifi,
    Bluetooth,
    Sound,
    /// The clock panel.
    Clock,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Watch(Watching),
    SetWifi(bool),
    /// Connects to a network, or disconnects if it's the active one.
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
    SetBluetooth(bool),
    /// Connects a device, or disconnects it if it's connected.
    ClickBluetooth {
        id: String,
    },
    SetAirplane(bool),
    SetBrightness(u8),
    SetVolume(u8),
    SetMuted(bool),
    SetMicMuted(bool),
    SetSink(String),
    SetPowerSaver(bool),
    SetDark(bool),
    Suspend,
    Restart,
    ShutDown,
    MediaPlayPause,
    MediaPrevious,
    MediaNext,
    /// Goes to a place in the track, from 0 to 1.
    MediaSeek(f32),
    NotificationClosed {
        id: u32,
        reason: CloseReason,
    },
    NotificationClicked {
        id: u32,
    },
}

fn proxy(
    bus: &Connection,
    destination: &'static str,
    path: &str,
    interface: &'static str,
) -> zbus::Result<Proxy<'static>> {
    Builder::new(bus)
        .destination(destination)?
        .path(path.to_owned())?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
}

impl Command {
    /// Whether doing this makes doing `earlier` pointless. These are the things that
    /// dragging a slider sends more of than can be kept up with.
    fn replaces(&self, earlier: &Self) -> bool {
        matches!(
            (self, earlier),
            (Self::SetBrightness(_), Self::SetBrightness(_))
                | (Self::SetVolume(_), Self::SetVolume(_))
                | (Self::MediaSeek(_), Self::MediaSeek(_))
        )
    }
}

/// The player to show: one that's playing if there is one, or else the first one.
fn choose_player(players: Vec<(String, bool)>) -> Option<String> {
    let playing = players.iter().position(|(_, playing)| *playing);
    let (name, _) = players.into_iter().nth(playing.unwrap_or(0))?;
    Some(name)
}

/// Turns a `file://` URL into a path, undoing the escaping of spaces and such.
fn file_url_path(url: &str) -> Option<PathBuf> {
    let escaped = url.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(escaped.len());
    let mut rest = escaped.as_bytes();

    while let Some((&byte, after)) = rest.split_first() {
        let decoded = (byte == b'%')
            .then(|| after.get(..2))
            .flatten()
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());

        match decoded {
            Some(decoded) => {
                bytes.push(decoded);
                rest = &after[2..];
            }
            None => {
                bytes.push(byte);
                rest = after;
            }
        }
    }

    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

fn load_art(path: &Path) -> Option<Arc<RgbaImage>> {
    let image = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .inspect_err(|err| tracing::debug!("Failed to load album art: {err}"))
        .ok()?;
    Some(Arc::new(image.thumbnail(ART_SIZE, ART_SIZE).into_rgba8()))
}

/// Accepts pairing for devices with nothing to confirm it on.
struct BluetoothAgent;

#[zbus::interface(name = "org.bluez.Agent1")]
impl BluetoothAgent {
    fn release(&self) {}

    fn request_authorization(&self, _device: OwnedObjectPath) {}

    fn authorize_service(&self, _device: OwnedObjectPath, _uuid: String) {}

    fn cancel(&self) {}
}

/// A connection to a Wi-Fi network that has yet to prove it works.
struct PendingWifi {
    ssid: String,
    connection: OwnedObjectPath,
    started: Instant,
}

/// What Bluetooth's threads leave behind.
#[derive(Default)]
struct BluetoothProgress {
    connecting: Option<String>,
    error: Option<String>,
}

struct Worker {
    bus: Option<Connection>,
    /// The session's bus, which apps are on.
    session: Option<Connection>,
    /// The album art last loaded, by its URL.
    art: Option<(String, Option<Arc<RgbaImage>>)>,
    watching: Watching,
    pending_wifi: Option<PendingWifi>,
    error: Option<String>,
    bluetooth: Arc<Mutex<BluetoothProgress>>,
    last_scan: Option<Instant>,
}

impl Worker {
    fn new(notify: impl Fn(notifications::Event) + Send + Sync + 'static) -> Self {
        let session = Connection::session()
            .inspect_err(|err| warn!("Failed to reach the session bus: {err}"))
            .ok();
        if let Some(session) = &session {
            notifications::serve(session, notify);
        }

        let bus = Connection::system()
            .inspect_err(|err| warn!("Failed to reach the system bus: {err}"))
            .ok();

        if let Some(bus) = &bus {
            let registered = bus
                .object_server()
                .at(BLUEZ_AGENT_PATH, BluetoothAgent)
                .and_then(|_| {
                    proxy(bus, BLUEZ, "/org/bluez", "org.bluez.AgentManager1")?.call::<_, _, ()>(
                        "RegisterAgent",
                        &(
                            ObjectPath::from_static_str_unchecked(BLUEZ_AGENT_PATH),
                            "NoInputNoOutput",
                        ),
                    )
                });
            if let Err(err) = registered {
                warn!("Failed to register a Bluetooth agent: {err}");
            }
        }

        Self {
            bus,
            session,
            art: None,
            watching: Watching::default(),
            pending_wifi: None,
            error: None,
            bluetooth: Default::default(),
            last_scan: None,
        }
    }

    /// The players on the session's bus, and whether they're playing.
    fn players(&self) -> zbus::Result<Vec<(String, bool)>> {
        let session = self.session.as_ref().ok_or(zbus::Error::Unsupported)?;
        let names: Vec<String> = proxy(
            session,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )?
        .call("ListNames", &())?;

        Ok(names
            .into_iter()
            .filter(|name| name.starts_with(MPRIS_PREFIX))
            .map(|name| {
                let playing = self
                    .player(&name)
                    .and_then(|player| Ok(player.get_property::<String>("PlaybackStatus")?))
                    .is_ok_and(|status| status == "Playing");
                (name, playing)
            })
            .collect())
    }

    fn player(&self, name: &str) -> zbus::Result<Proxy<'static>> {
        let session = self.session.as_ref().ok_or(zbus::Error::Unsupported)?;
        Builder::new(session)
            .destination(name.to_owned())?
            .path(MPRIS_PATH)?
            .interface(MPRIS_PLAYER)?
            .cache_properties(CacheProperties::No)
            .build()
    }

    fn current_player(&self) -> zbus::Result<Proxy<'static>> {
        let name = choose_player(self.players()?).ok_or(zbus::Error::Unsupported)?;
        self.player(&name)
    }

    fn read_media(&mut self) -> Option<Media> {
        let player = self.current_player().ok()?;
        let metadata: HashMap<String, OwnedValue> = player.get_property("Metadata").ok()?;

        let text = |key: &str| {
            metadata
                .get(key)
                .and_then(|value| <&str>::try_from(value).ok())
                .map(str::to_owned)
                .unwrap_or_default()
        };
        let number = |key: &str| {
            metadata.get(key).and_then(|value| {
                i64::try_from(value)
                    .ok()
                    .or_else(|| u64::try_from(value).ok().map(|value| value as i64))
            })
        };

        let title = text("xesam:title");
        if title.is_empty() {
            return None;
        }

        let artist = metadata
            .get("xesam:artist")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| Vec::<String>::try_from(value).ok())
            .unwrap_or_default()
            .join(", ");

        let progress = player
            .get_property::<i64>("Position")
            .ok()
            .zip(number("mpris:length").filter(|length| *length > 0))
            .map(|(position, length)| (position as f32 / length as f32).clamp(0.0, 1.0));

        let art_url = text("mpris:artUrl");
        if self.art.as_ref().is_none_or(|(url, _)| *url != art_url) {
            let art = file_url_path(&art_url).and_then(|path| load_art(&path));
            self.art = Some((art_url.clone(), art));
        }

        Some(Media {
            title,
            artist,
            playing: player.get_property::<String>("PlaybackStatus").ok()? == "Playing",
            progress,
            art_url,
            art: self.art.as_ref().and_then(|(_, art)| art.clone()),
        })
    }

    fn seek(&self, progress: f32) -> zbus::Result<()> {
        let player = self.current_player()?;
        let metadata: HashMap<String, OwnedValue> = player.get_property("Metadata")?;
        let length = metadata
            .get("mpris:length")
            .and_then(|value| {
                i64::try_from(value)
                    .ok()
                    .or_else(|| u64::try_from(value).ok().map(|value| value as i64))
            })
            .ok_or(zbus::Error::Unsupported)?;
        let track = metadata
            .get("mpris:trackid")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| OwnedObjectPath::try_from(value).ok())
            .ok_or(zbus::Error::Unsupported)?;

        player.call("SetPosition", &(track, (length as f32 * progress) as i64))
    }

    fn nm(&self) -> zbus::Result<Proxy<'_>> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        proxy(bus, NM, NM_PATH, NM)
    }

    fn wifi_device(&self) -> zbus::Result<OwnedObjectPath> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        let devices: Vec<OwnedObjectPath> = self.nm()?.call("GetDevices", &())?;

        devices
            .into_iter()
            .find(|path| {
                proxy(bus, NM, path.as_str(), NM_DEVICE)
                    .and_then(|device| device.get_property::<u32>("DeviceType"))
                    .is_ok_and(|kind| kind == NM_DEVICE_TYPE_WIFI)
            })
            .ok_or(zbus::Error::Unsupported)
    }

    /// The saved connection of each SSID.
    fn saved_networks(&self) -> zbus::Result<HashMap<Vec<u8>, OwnedObjectPath>> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        let settings = proxy(bus, NM, format!("{NM_PATH}/Settings").as_str(), NM_SETTINGS)?;
        let connections: Vec<OwnedObjectPath> = settings.call("ListConnections", &())?;

        Ok(connections
            .into_iter()
            .filter_map(|path| {
                let connection = proxy(bus, NM, path.as_str(), NM_CONNECTION).ok()?;
                let settings: HashMap<String, HashMap<String, OwnedValue>> =
                    connection.call("GetSettings", &()).ok()?;
                let ssid = settings.get("802-11-wireless")?.get("ssid")?;
                Some((Vec::<u8>::try_from(ssid.try_clone().ok()?).ok()?, path))
            })
            .collect())
    }

    fn read_networks(&self) -> zbus::Result<Vec<Network>> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        let device_path = self.wifi_device()?;
        let wireless = proxy(bus, NM, device_path.as_str(), NM_WIRELESS)?;
        let device = proxy(bus, NM, device_path.as_str(), NM_DEVICE)?;

        let state: u32 = device.get_property("State")?;
        let active_ap: OwnedObjectPath = wireless.get_property("ActiveAccessPoint")?;
        let access_points: Vec<OwnedObjectPath> = wireless.call("GetAllAccessPoints", &())?;
        let saved = self.saved_networks()?;

        let mut networks = Vec::<Network>::new();
        for path in access_points {
            let ap = proxy(bus, NM, path.as_str(), NM_AP)?;
            let ssid: Vec<u8> = ap.get_property("Ssid")?;
            // Hidden networks have no name to show.
            if ssid.is_empty() {
                continue;
            }

            let flags = ap.get_property::<u32>("Flags")?;
            let wpa = ap.get_property::<u32>("WpaFlags")?;
            let rsn = ap.get_property::<u32>("RsnFlags")?;
            let is_active = path == active_ap;

            let network = Network {
                ssid: String::from_utf8_lossy(&ssid).into_owned(),
                strength: ap.get_property("Strength")?,
                secured: flags != 0 || wpa != 0 || rsn != 0,
                active: is_active && state == NM_STATE_ACTIVATED,
                known: saved.contains_key(&ssid),
                connecting: is_active && (NM_STATE_PREPARE..NM_STATE_ACTIVATED).contains(&state),
            };

            match networks.iter_mut().find(|n| n.ssid == network.ssid) {
                Some(other) => {
                    other.strength = other.strength.max(network.strength);
                    other.active |= network.active;
                    other.connecting |= network.connecting;
                }
                None => networks.push(network),
            }
        }

        sort_networks(&mut networks);
        Ok(networks)
    }

    /// The name of the network that's connected.
    fn read_wifi_name(&self) -> zbus::Result<String> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        let device = self.wifi_device()?;
        let active: OwnedObjectPath =
            proxy(bus, NM, device.as_str(), NM_WIRELESS)?.get_property("ActiveAccessPoint")?;
        let ssid: Vec<u8> = proxy(bus, NM, active.as_str(), NM_AP)?.get_property("Ssid")?;
        Ok(String::from_utf8_lossy(&ssid).into_owned())
    }

    fn click_network(&mut self, ssid: &str) -> zbus::Result<()> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        let device = self.wifi_device()?;

        if self
            .read_networks()?
            .iter()
            .any(|n| n.ssid == ssid && n.active)
        {
            return proxy(bus, NM, device.as_str(), NM_DEVICE)?.call("Disconnect", &());
        }

        match self.saved_networks()?.get(ssid.as_bytes()) {
            Some(connection) => self
                .nm()?
                .call::<_, _, OwnedObjectPath>(
                    "ActivateConnection",
                    &(
                        connection,
                        &device,
                        ObjectPath::from_static_str_unchecked("/"),
                    ),
                )
                .map(drop),
            None => self.connect_new(ssid, None),
        }
    }

    /// Adds a connection for a network that doesn't have one yet.
    fn connect_new(&mut self, ssid: &str, password: Option<String>) -> zbus::Result<()> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        let device = self.wifi_device()?;
        let wireless = proxy(bus, NM, device.as_str(), NM_WIRELESS)?;
        let access_points: Vec<OwnedObjectPath> = wireless.call("GetAllAccessPoints", &())?;

        let access_point = access_points
            .into_iter()
            .find(|path| {
                proxy(bus, NM, path.as_str(), NM_AP)
                    .and_then(|ap| ap.get_property::<Vec<u8>>("Ssid"))
                    .is_ok_and(|found| found == ssid.as_bytes())
            })
            .ok_or_else(|| zbus::Error::Failure(format!("{ssid} is out of range")))?;

        let mut settings: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();
        if let Some(password) = password {
            let ap = proxy(bus, NM, access_point.as_str(), NM_AP)?;
            let flags = ap.get_property::<u32>("WpaFlags")? | ap.get_property::<u32>("RsnFlags")?;
            let key_mgmt = key_management(flags)
                .ok_or_else(|| zbus::Error::Failure(format!("{ssid} needs an enterprise login")))?;
            settings.insert(
                "802-11-wireless-security",
                HashMap::from([
                    ("key-mgmt", Value::from(key_mgmt)),
                    ("psk", Value::from(password)),
                ]),
            );
        }

        let (connection, _active): (OwnedObjectPath, OwnedObjectPath) = self.nm()?.call(
            "AddAndActivateConnection",
            &(settings, &device, &access_point),
        )?;

        self.pending_wifi = Some(PendingWifi {
            ssid: ssid.to_owned(),
            connection,
            started: Instant::now(),
        });
        Ok(())
    }

    /// Removes a new connection that didn't work, such as with a wrong password,
    /// so that the network doesn't look saved.
    fn check_pending_wifi(&mut self) {
        let Some(pending) = self.pending_wifi.as_ref() else {
            return;
        };
        let (Some(bus), Ok(device)) = (self.bus.as_ref(), self.wifi_device()) else {
            return;
        };
        let Ok(state) = proxy(bus, NM, device.as_str(), NM_DEVICE)
            .and_then(|device| device.get_property::<u32>("State"))
        else {
            return;
        };

        let failed = state == NM_STATE_FAILED
            || (state <= NM_STATE_DISCONNECTED && pending.started.elapsed() > CONNECT_GRACE);

        if state == NM_STATE_ACTIVATED {
            self.pending_wifi = None;
        } else if failed {
            if let Ok(connection) = proxy(bus, NM, pending.connection.as_str(), NM_CONNECTION) {
                let _: zbus::Result<()> = connection.call("Delete", &());
            }
            self.error = Some(format!("Couldn't connect to {}", pending.ssid));
            self.pending_wifi = None;
        }
    }

    fn bluez_objects(
        &self,
    ) -> zbus::Result<HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        proxy(bus, BLUEZ, "/", "org.freedesktop.DBus.ObjectManager")?.call("GetManagedObjects", &())
    }

    fn adapter(&self) -> zbus::Result<OwnedObjectPath> {
        self.bluez_objects()?
            .into_iter()
            .find_map(|(path, interfaces)| interfaces.contains_key(BLUEZ_ADAPTER).then_some(path))
            .ok_or(zbus::Error::Unsupported)
    }

    fn read_bluetooth(&self) -> zbus::Result<(bool, Vec<BluetoothDevice>)> {
        let objects = self.bluez_objects()?;
        let connecting = self.bluetooth.lock().unwrap().connecting.clone();

        let powered = objects.values().any(|interfaces| {
            interfaces
                .get(BLUEZ_ADAPTER)
                .and_then(|adapter| bool::try_from(adapter.get("Powered")?).ok())
                .unwrap_or(false)
        }) && !status::is_blocked("bluetooth");

        let mut devices = objects
            .iter()
            .filter_map(|(path, interfaces)| {
                let device = interfaces.get(BLUEZ_DEVICE)?;
                let text = |name: &str| {
                    device
                        .get(name)
                        .and_then(|value| <&str>::try_from(value).ok())
                        .map(str::to_owned)
                };
                let flag = |name: &str| {
                    device
                        .get(name)
                        .and_then(|value| bool::try_from(value).ok())
                        .unwrap_or(false)
                };

                Some(BluetoothDevice {
                    id: path.to_string(),
                    // Devices without a name are of little use in a list.
                    name: text("Name").or_else(|| text("Alias").filter(|_| flag("Paired")))?,
                    icon: text("Icon").unwrap_or_default(),
                    connected: flag("Connected"),
                    paired: flag("Paired"),
                    connecting: connecting.as_deref() == Some(path.as_str()),
                    battery: interfaces
                        .get(BLUEZ_BATTERY)
                        .and_then(|battery| u8::try_from(battery.get("Percentage")?).ok()),
                })
            })
            .collect::<Vec<_>>();

        devices.sort_by_key(|device| {
            (
                !device.connected,
                !device.paired,
                device.name.to_lowercase(),
            )
        });

        Ok((powered, devices))
    }

    fn set_discovering(&self, discovering: bool) {
        let result = (|| -> zbus::Result<()> {
            let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
            let adapter = self.adapter()?;
            let adapter = proxy(bus, BLUEZ, adapter.as_str(), BLUEZ_ADAPTER)?;
            adapter.call(
                if discovering { "StartDiscovery" } else { "StopDiscovery" },
                &(),
            )
        })();

        // This also fails when there's nothing to change, and when Bluetooth is off.
        if let Err(err) = result {
            tracing::debug!("Failed to change Bluetooth discovery: {err}");
        }
    }

    fn set_bluetooth(&self, enabled: bool) -> zbus::Result<()> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;

        if enabled {
            // Powering on doesn't work while it's blocked.
            if let Err(err) = fs::write(
                "/dev/rfkill",
                [
                    0,
                    0,
                    0,
                    0,
                    RFKILL_TYPE_BLUETOOTH,
                    RFKILL_OP_CHANGE_ALL,
                    0,
                    0,
                ],
            ) {
                warn!("Failed to unblock Bluetooth: {err}");
            }
        }

        let adapter = self.adapter()?;
        proxy(bus, BLUEZ, adapter.as_str(), BLUEZ_ADAPTER)?.set_property("Powered", enabled)?;
        Ok(())
    }

    fn click_bluetooth(&self, id: String) -> zbus::Result<()> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?.clone();
        let objects = self.bluez_objects()?;
        let device = objects
            .iter()
            .find(|(object, _)| object.as_str() == id)
            .and_then(|(_, interfaces)| interfaces.get(BLUEZ_DEVICE))
            .ok_or(zbus::Error::Unsupported)?;
        let flag = |name: &str| {
            device
                .get(name)
                .and_then(|value| bool::try_from(value).ok())
                .unwrap_or(false)
        };
        let (connected, paired) = (flag("Connected"), flag("Paired"));

        self.bluetooth.lock().unwrap().connecting = Some(id.clone());
        let progress = self.bluetooth.clone();

        // Connecting can take half a minute.
        let spawned = thread::Builder::new()
            .name("shell-bluetooth".into())
            .spawn(move || {
                let result = (|| -> zbus::Result<()> {
                    let device = proxy(&bus, BLUEZ, id.as_str(), BLUEZ_DEVICE)?;
                    if connected {
                        return device.call("Disconnect", &());
                    }
                    if !paired {
                        device.call::<_, _, ()>("Pair", &())?;
                        device.set_property("Trusted", true)?;
                    }
                    device.call("Connect", &())
                })();

                let mut progress = progress.lock().unwrap();
                progress.connecting = None;
                progress.error = result
                    .inspect_err(|err| warn!("Bluetooth failed: {err}"))
                    .err()
                    .map(|_| "Couldn't connect the device".to_owned());
            });

        if let Err(err) = spawned {
            warn!("Failed to start a thread: {err}");
        }
        Ok(())
    }

    /// The backlight's name, value and maximum.
    fn read_backlight() -> Option<(String, u32, u32)> {
        let entry = fs::read_dir("/sys/class/backlight")
            .ok()?
            .flatten()
            .next()?;
        let read = |name: &str| -> Option<u32> {
            fs::read_to_string(entry.path().join(name))
                .ok()?
                .trim()
                .parse()
                .ok()
        };
        Some((
            entry.file_name().into_string().ok()?,
            read("brightness")?,
            read("max_brightness").filter(|max| *max > 0)?,
        ))
    }

    /// Goes through logind, because only root can write to the backlight.
    fn set_brightness(&self, percent: u8) -> zbus::Result<()> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        let (name, _, max) = Self::read_backlight().ok_or(zbus::Error::Unsupported)?;

        let user = proxy(
            bus,
            LOGIND,
            format!("{LOGIND_PATH}/user/self").as_str(),
            "org.freedesktop.login1.User",
        )?;
        let (_, session): (String, OwnedObjectPath) = user.get_property("Display")?;
        let value = (max as u64 * percent as u64 / 100).max(1) as u32;

        proxy(
            bus,
            LOGIND,
            session.as_str(),
            "org.freedesktop.login1.Session",
        )?
        .call("SetBrightness", &("backlight", name, value))
    }

    fn logind_call(&self, method: &str) -> zbus::Result<()> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        proxy(bus, LOGIND, LOGIND_PATH, "org.freedesktop.login1.Manager")?.call(method, &(true,))
    }

    fn power_profiles(&self) -> zbus::Result<Proxy<'_>> {
        let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
        proxy(bus, POWER_PROFILES, POWER_PROFILES_PATH, POWER_PROFILES)
    }

    fn run(&mut self, command: Command) -> zbus::Result<()> {
        match command {
            Command::Watch(watching) => {
                if (self.watching == Watching::Bluetooth) != (watching == Watching::Bluetooth) {
                    self.set_discovering(watching == Watching::Bluetooth);
                }
                if watching == Watching::Wifi {
                    self.last_scan = None;
                }
                self.watching = watching;
                self.error = None;
                Ok(())
            }
            Command::SetWifi(enabled) => self
                .nm()?
                .set_property("WirelessEnabled", enabled)
                .map_err(Into::into),
            Command::ClickNetwork { ssid } => self.click_network(&ssid),
            Command::ConnectWifi { ssid, password } => self.connect_new(&ssid, Some(password)),
            Command::ForgetWifi { ssid } => {
                let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
                match self.saved_networks()?.get(ssid.as_bytes()) {
                    Some(path) => proxy(bus, NM, path.as_str(), NM_CONNECTION)?.call("Delete", &()),
                    None => Ok(()),
                }
            }
            Command::SetBluetooth(enabled) => self.set_bluetooth(enabled),
            Command::ClickBluetooth { id } => self.click_bluetooth(id),
            Command::SetAirplane(enabled) => {
                self.nm()?.set_property("WirelessEnabled", !enabled)?;
                self.set_bluetooth(!enabled)
            }
            Command::SetBrightness(percent) => self.set_brightness(percent),
            Command::SetVolume(percent) => {
                pactl(&["set-sink-volume", "@DEFAULT_SINK@", &format!("{percent}%")]);
                Ok(())
            }
            Command::SetMuted(muted) => {
                pactl(&[
                    "set-sink-mute",
                    "@DEFAULT_SINK@",
                    if muted { "1" } else { "0" },
                ]);
                Ok(())
            }
            Command::SetMicMuted(muted) => {
                pactl(&[
                    "set-source-mute",
                    "@DEFAULT_SOURCE@",
                    if muted { "1" } else { "0" },
                ]);
                Ok(())
            }
            Command::SetSink(name) => {
                pactl(&["set-default-sink", &name]);
                Ok(())
            }
            Command::SetPowerSaver(enabled) => {
                let profile = if enabled { "power-saver" } else { "balanced" };
                self.power_profiles()?
                    .set_property("ActiveProfile", profile)
                    .map_err(Into::into)
            }
            Command::SetDark(dark) => {
                set_dark(dark);
                Ok(())
            }
            Command::Suspend => self.logind_call("Suspend"),
            Command::Restart => self.logind_call("Reboot"),
            Command::ShutDown => self.logind_call("PowerOff"),
            Command::MediaPlayPause => self.current_player()?.call("PlayPause", &()),
            Command::MediaPrevious => self.current_player()?.call("Previous", &()),
            Command::MediaNext => self.current_player()?.call("Next", &()),
            Command::MediaSeek(progress) => self.seek(progress),
            Command::NotificationClosed { id, reason } => {
                if let Some(session) = &self.session {
                    notifications::emit_closed(session, id, reason);
                }
                Ok(())
            }
            Command::NotificationClicked { id } => {
                if let Some(session) = &self.session {
                    notifications::emit_clicked(session, id);
                }
                Ok(())
            }
        }
    }

    /// Reads the state of what's being watched.
    fn read(&mut self) -> System {
        self.check_pending_wifi();

        let watching = self.watching;

        if watching == Watching::Wifi
            && self
                .last_scan
                .is_none_or(|scan| scan.elapsed() >= SCAN_INTERVAL)
        {
            self.last_scan = Some(Instant::now());
            let scan = self.wifi_device().and_then(|device| {
                let bus = self.bus.as_ref().ok_or(zbus::Error::Unsupported)?;
                proxy(bus, NM, device.as_str(), NM_WIRELESS)?
                    .call::<_, _, ()>("RequestScan", &(HashMap::<&str, Value<'_>>::new(),))
            });
            // This fails if a scan is already going on, which is just as good.
            if let Err(err) = scan {
                tracing::debug!("Failed to scan for Wi-Fi networks: {err}");
            }
        }

        let wifi_enabled = self
            .nm()
            .and_then(|nm| {
                nm.get_property::<bool>("WirelessEnabled")
                    .map_err(Into::into)
            })
            .unwrap_or(false);

        let (bluetooth_enabled, bluetooth_devices) = self.read_bluetooth().unwrap_or_default();

        let (brightness, sinks) = (
            Self::read_backlight().map(|(_, value, max)| (value as u64 * 100 / max as u64) as u8),
            // The audio tile says where sound is playing, so this is always of use.
            read_sinks(),
        );

        let error = {
            let mut progress = self.bluetooth.lock().unwrap();
            progress.error.take().or_else(|| self.error.clone())
        };
        self.error = error.clone();

        let media = if watching == Watching::Clock {
            self.read_media()
        } else {
            None
        };

        System {
            media,
            wifi_enabled,
            wifi_name: self.read_wifi_name().ok(),
            networks: if watching == Watching::Wifi {
                self.read_networks().unwrap_or_default()
            } else {
                Vec::new()
            },
            bluetooth_enabled,
            bluetooth_name: bluetooth_devices
                .iter()
                .find(|device| device.connected)
                .map(|device| device.name.clone()),
            bluetooth_devices: if watching == Watching::Bluetooth {
                bluetooth_devices
            } else {
                Vec::new()
            },
            sinks,
            brightness,
            mic_muted: pactl(&["get-source-mute", "@DEFAULT_SOURCE@"])
                .is_some_and(|output| output.trim() == "Mute: yes"),
            power_saver: self
                .power_profiles()
                .and_then(|profiles| {
                    profiles
                        .get_property::<String>("ActiveProfile")
                        .map_err(Into::into)
                })
                .ok()
                .map(|profile| profile == "power-saver"),
            dark: read_dark(),
            error,
        }
    }
}

/// Connected and saved networks first, then the strongest ones.
fn sort_networks(networks: &mut [Network]) {
    networks.sort_by_key(|network| {
        (
            !(network.active || network.connecting),
            !network.known,
            std::cmp::Reverse(network.strength),
        )
    });
}

/// The `key-mgmt` setting for a network with these security flags, which is
/// `None` for ones that don't work with just a password.
fn key_management(flags: u32) -> Option<&'static str> {
    if flags & AP_KEY_MGMT_PSK != 0 {
        Some("wpa-psk")
    } else if flags & AP_KEY_MGMT_SAE != 0 {
        Some("sae")
    } else if flags & AP_KEY_MGMT_802_1X != 0 {
        None
    } else {
        // WEP is all that's left.
        Some("none")
    }
}

fn pactl(args: &[&str]) -> Option<String> {
    let output = Process::new("pactl")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parses `pactl list sinks` into names and descriptions.
fn parse_sinks(list: &str) -> Vec<(String, String)> {
    let mut name = None;
    let mut sinks = Vec::new();

    for line in list.lines().map(str::trim) {
        if let Some(found) = line.strip_prefix("Name: ") {
            name = Some(found.to_owned());
        } else if let Some(description) = line.strip_prefix("Description: ")
            && let Some(name) = name.take()
        {
            sinks.push((name, description.to_owned()));
        }
    }

    sinks
}

fn read_sinks() -> Vec<Sink> {
    let default = pactl(&["get-default-sink"]).unwrap_or_default();

    pactl(&["list", "sinks"])
        .map(|list| parse_sinks(&list))
        .unwrap_or_default()
        .into_iter()
        .map(|(name, description)| Sink {
            active: name == default.trim(),
            name,
            description,
        })
        .collect()
}

pub fn gsettings(args: &[&str]) -> Option<String> {
    let output = Process::new("gsettings").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";

/// Whether apps were told to be dark, which is dark by default if nothing says.
pub fn read_dark() -> bool {
    gsettings(&["get", INTERFACE_SCHEMA, "color-scheme"])
        .is_none_or(|scheme| scheme.trim_matches('\'') != "default")
}

fn set_dark(dark: bool) {
    let scheme = if dark { "prefer-dark" } else { "default" };
    gsettings(&["set", INTERFACE_SCHEMA, "color-scheme", scheme]);

    // Apps using plain GTK only look at this, by way of a `-dark` theme.
    if let Some(theme) = gsettings(&["get", INTERFACE_SCHEMA, "gtk-theme"]) {
        let theme = theme.trim_matches('\'').trim_end_matches("-dark");
        let theme = if dark {
            format!("{theme}-dark")
        } else {
            theme.to_owned()
        };
        gsettings(&["set", INTERFACE_SCHEMA, "gtk-theme", &theme]);
    }
}

/// Runs the worker until a `send` returns false.
pub fn spawn(
    commands: Receiver<Command>,
    send_status: impl Fn(Status) -> bool + Send + 'static,
    send_system: impl Fn(System) -> bool + Send + 'static,
    notify: impl Fn(notifications::Event) + Send + Sync + 'static,
) {
    let spawned = thread::Builder::new()
        .name("shell-system".into())
        .spawn(move || {
            let mut worker = Worker::new(notify);
            let mut reader = status::Reader::new(worker.bus.clone());
            // The theme depends on this, so don't wait for quick settings to open.
            if !send_system(worker.read()) {
                return;
            }
            let mut last_status = Instant::now();
            let mut last_read = Instant::now();
            let mut next_read = IDLE_INTERVAL;
            // What was just changed may be part of the status, and sliders wait for that.
            let mut status_outdated = false;

            loop {
                let open = worker.watching != Watching::Nothing;
                let wait = next_read.saturating_sub(last_read.elapsed());

                match commands.recv_timeout(wait) {
                    Ok(command) => {
                        // A dragged slider sends more than can be kept up with.
                        let mut command = command;
                        while let Ok(next) = commands.try_recv() {
                            if !next.replaces(&command) {
                                run(&mut worker, command);
                            }
                            command = next;
                        }

                        let was_open = open;
                        run(&mut worker, command);
                        next_read = AFTER_COMMAND;
                        status_outdated = true;

                        // Don't keep anyone waiting for what opening shows.
                        if !was_open && worker.watching != Watching::Nothing {
                            last_read = Instant::now() - AFTER_COMMAND;
                        } else {
                            last_read = Instant::now();
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => (),
                    Err(RecvTimeoutError::Disconnected) => return,
                }

                let open = worker.watching != Watching::Nothing;
                last_read = Instant::now();
                next_read = if open { OPEN_INTERVAL } else { IDLE_INTERVAL };

                if open && !send_system(worker.read()) {
                    return;
                }

                if status_outdated || last_status.elapsed() >= STATUS_INTERVAL {
                    status_outdated = false;
                    last_status = Instant::now();
                    if !send_status(reader.read()) {
                        return;
                    }
                }
            }
        });

    if let Err(err) = spawned {
        tracing::error!("Failed to start the shell's worker: {err}");
    }
}

fn run(worker: &mut Worker, command: Command) {
    if let Err(err) = worker.run(command) {
        warn!("Quick settings failed: {err}");
        worker.error = Some(user_message(&err));
    }
}

/// Makes the part of `err` that's meant for people presentable.
fn user_message(err: &zbus::Error) -> String {
    match err {
        zbus::Error::Failure(message) => message.clone(),
        _ => "That didn't work".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sliders_replace_each_other() {
        assert!(Command::SetVolume(1).replaces(&Command::SetVolume(2)));
        assert!(!Command::SetVolume(1).replaces(&Command::SetBrightness(2)));

        let closed = |id| Command::NotificationClosed {
            id,
            reason: CloseReason::Dismissed,
        };
        assert!(!closed(1).replaces(&closed(2)));
    }

    #[test]
    fn players() {
        let players = |list: &[(&str, bool)]| {
            list.iter()
                .map(|(name, playing)| (name.to_string(), *playing))
                .collect::<Vec<_>>()
        };

        assert_eq!(choose_player(Vec::new()), None);
        assert_eq!(
            choose_player(players(&[("a", false), ("b", true)])).as_deref(),
            Some("b")
        );
        assert_eq!(
            choose_player(players(&[("a", false), ("b", false)])).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn file_urls() {
        assert_eq!(
            file_url_path("file:///tmp/My%20Song.png"),
            Some(PathBuf::from("/tmp/My Song.png"))
        );
        assert_eq!(file_url_path("https://example.com/a.png"), None);
        assert_eq!(file_url_path("file:///a%2"), Some(PathBuf::from("/a%2")));
    }

    #[test]
    fn sinks() {
        let list = concat!(
            "Sink #0\n\tState: IDLE\n\tName: alsa\n\tDescription: Built-in\n",
            "\tDriver: x\n\tProperties:\n\t\tdevice.description = \"Name: no\"\n",
            "Sink #1\n\tName: buds\n\tDescription: Buds Pro\n",
        );
        assert_eq!(
            parse_sinks(list),
            vec![
                ("alsa".to_owned(), "Built-in".to_owned()),
                ("buds".to_owned(), "Buds Pro".to_owned()),
            ]
        );
    }

    #[test]
    fn network_order() {
        let network = |ssid: &str, strength, known, active| Network {
            ssid: ssid.to_owned(),
            strength,
            secured: true,
            active,
            known,
            connecting: false,
        };
        let mut networks = [
            network("strong", 90, false, false),
            network("saved", 20, true, false),
            network("active", 10, true, true),
            network("weak", 30, false, false),
        ];
        sort_networks(&mut networks);
        let names = networks.iter().map(|n| n.ssid.as_str()).collect::<Vec<_>>();
        assert_eq!(names, ["active", "saved", "strong", "weak"]);
    }

    #[test]
    fn key_mgmt() {
        assert_eq!(key_management(AP_KEY_MGMT_PSK), Some("wpa-psk"));
        // Networks offering both keep working with older hardware.
        assert_eq!(
            key_management(AP_KEY_MGMT_PSK | AP_KEY_MGMT_SAE),
            Some("wpa-psk")
        );
        assert_eq!(key_management(AP_KEY_MGMT_SAE), Some("sae"));
        assert_eq!(key_management(AP_KEY_MGMT_802_1X), None);
        assert_eq!(key_management(0), Some("none"));
    }
}
