// SPDX-License-Identifier: GPL-3.0-or-later

//! The settings app.

use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

use image::RgbaImage;
use slint::{Image, ModelRc, PlatformError, Rgba8Pixel, SharedPixelBuffer, VecModel};
use smithay::{
    input::keyboard::{KeysymHandle, keysyms},
    utils::{Logical, Point, Rectangle, Size},
};

use super::{
    BAR_HEIGHT, BluetoothItem, InfoItem, NetworkItem, OutputItem, SettingsApp, SettingsPage, Shell,
    ShellWindow, SinkItem, TopicItem, WallpaperItem, about, search, set_platform, status,
    system::Command, update_model,
};
use crate::render::util::blur::Blur;

/// The size the app tries to be in logical pixels.
const WIDTH: i32 = 960;
const HEIGHT: i32 = 680;

const MARGIN: i32 = 8;

/// The pages in the order of [`search::TOPICS`].
pub const PAGES: [SettingsPage; 8] = [
    SettingsPage::Network,
    SettingsPage::Devices,
    SettingsPage::Display,
    SettingsPage::Style,
    SettingsPage::Sound,
    SettingsPage::Notifications,
    SettingsPage::Battery,
    SettingsPage::About,
];

/// What was done in the settings app.
pub enum SettingsAction {
    Close,
    Query,
    OpenPage(SettingsPage),
    ToggleWifi,
    ToggleBluetooth,
    ToggleDark,
    ToggleNightLight,
    ToggleMute,
    ToggleMicrophone,
    ToggleDoNotDisturb,
    TogglePowerSaver,
    ToggleStayAwake,
    ClearNotifications,
    SetScale(String, f32),
    SetWallpaper(PathBuf),
    Run(Command),
}

/// The size of the app on an output of this size.
pub fn size(output: Size<i32, Logical>) -> Size<i32, Logical> {
    (
        WIDTH.min(output.w - MARGIN * 2),
        HEIGHT.min(output.h - BAR_HEIGHT - MARGIN * 2),
    )
        .into()
}

/// An output's details for the display page.
pub struct OutputDetails {
    pub name: String,
    pub resolution: String,
    pub scale: f64,
}

/// The settings app on an output.
pub struct OutputSettings {
    pub ui: SettingsApp,
    pub window: ShellWindow,
    pub blur: RefCell<Blur>,
    topics: Rc<VecModel<TopicItem>>,
    networks: Rc<VecModel<NetworkItem>>,
    bluetooth_devices: Rc<VecModel<BluetoothItem>>,
    sinks: Rc<VecModel<SinkItem>>,
    outputs: Rc<VecModel<OutputItem>>,
    wallpapers: Rc<VecModel<WallpaperItem>>,
    actions: Rc<RefCell<Vec<SettingsAction>>>,
    thumbnails: RefCell<HashMap<PathBuf, Image>>,
}

impl std::fmt::Debug for OutputSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputSettings").finish_non_exhaustive()
    }
}

impl OutputSettings {
    pub fn new(size: Size<i32, Logical>, scale: f64) -> Result<Self, PlatformError> {
        set_platform();
        let ui = SettingsApp::new()?;
        let window = ShellWindow::new(&ui, size, scale)?;

        let topics = Rc::new(VecModel::default());
        ui.set_topics(ModelRc::from(topics.clone()));
        let networks = Rc::new(VecModel::default());
        ui.set_networks(ModelRc::from(networks.clone()));
        let bluetooth_devices = Rc::new(VecModel::default());
        ui.set_bluetooth_devices(ModelRc::from(bluetooth_devices.clone()));
        let sinks = Rc::new(VecModel::default());
        ui.set_sinks(ModelRc::from(sinks.clone()));
        let outputs = Rc::new(VecModel::default());
        ui.set_outputs(ModelRc::from(outputs.clone()));
        let wallpapers = Rc::new(VecModel::default());
        ui.set_wallpapers(ModelRc::from(wallpapers.clone()));
        ui.set_about(ModelRc::from(Rc::new(VecModel::from(
            about::system_info()
                .into_iter()
                .map(|(title, value)| InfoItem {
                    title: title.into(),
                    value: value.into(),
                })
                .collect::<Vec<_>>(),
        ))));

        let actions = Rc::new(RefCell::new(Vec::new()));
        let queue = |make: fn() -> SettingsAction| {
            let actions = actions.clone();
            move || actions.borrow_mut().push(make())
        };
        let run = |make: fn(String) -> Command| {
            let actions = actions.clone();
            move |text: slint::SharedString| {
                actions
                    .borrow_mut()
                    .push(SettingsAction::Run(make(text.into())));
            }
        };
        let percent = |make: fn(u8) -> Command| {
            let actions = actions.clone();
            move |value: f32| {
                let percent = (value * 100.0).round() as u8;
                actions
                    .borrow_mut()
                    .push(SettingsAction::Run(make(percent)));
            }
        };

        ui.on_closed(queue(|| SettingsAction::Close));
        ui.on_wifi_toggled(queue(|| SettingsAction::ToggleWifi));
        ui.on_bluetooth_toggled(queue(|| SettingsAction::ToggleBluetooth));
        ui.on_dark_toggled(queue(|| SettingsAction::ToggleDark));
        ui.on_night_light_toggled(queue(|| SettingsAction::ToggleNightLight));
        ui.on_mute_toggled(queue(|| SettingsAction::ToggleMute));
        ui.on_mic_toggled(queue(|| SettingsAction::ToggleMicrophone));
        ui.on_do_not_disturb_toggled(queue(|| SettingsAction::ToggleDoNotDisturb));
        ui.on_notifications_cleared(queue(|| SettingsAction::ClearNotifications));
        ui.on_power_saver_toggled(queue(|| SettingsAction::TogglePowerSaver));
        ui.on_stay_awake_toggled(queue(|| SettingsAction::ToggleStayAwake));
        ui.on_network_clicked(run(|ssid| Command::ClickNetwork { ssid }));
        ui.on_network_forgotten(run(|ssid| Command::ForgetWifi { ssid }));
        ui.on_bluetooth_clicked(run(|id| Command::ClickBluetooth { id }));
        ui.on_sink_chosen(run(Command::SetSink));
        ui.on_brightness_changed(percent(Command::SetBrightness));
        ui.on_volume_changed(percent(Command::SetVolume));
        ui.on_query_changed({
            let actions = actions.clone();
            move |_| actions.borrow_mut().push(SettingsAction::Query)
        });
        ui.on_page_chosen({
            let actions = actions.clone();
            move |page| actions.borrow_mut().push(SettingsAction::OpenPage(page))
        });
        ui.on_password_entered({
            let actions = actions.clone();
            move |ssid, password| {
                actions
                    .borrow_mut()
                    .push(SettingsAction::Run(Command::ConnectWifi {
                        ssid: ssid.into(),
                        password: password.into(),
                    }));
            }
        });
        ui.on_scale_chosen({
            let actions = actions.clone();
            move |name, scale| {
                actions
                    .borrow_mut()
                    .push(SettingsAction::SetScale(name.into(), scale));
            }
        });
        ui.on_wallpaper_chosen({
            let actions = actions.clone();
            move |path| {
                actions
                    .borrow_mut()
                    .push(SettingsAction::SetWallpaper(path.as_str().into()));
            }
        });

        ui.set_open(true);
        ui.invoke_focus_search();

        Ok(Self {
            ui,
            window,
            blur: Default::default(),
            topics,
            networks,
            bluetooth_devices,
            sinks,
            outputs,
            wallpapers,
            actions,
            thumbnails: RefCell::default(),
        })
    }

    pub fn take_actions(&self) -> Vec<SettingsAction> {
        std::mem::take(&mut self.actions.borrow_mut())
    }

    pub fn location(&self, output_width: i32) -> Point<i32, Logical> {
        ((output_width - self.window.size.w) / 2, BAR_HEIGHT + MARGIN).into()
    }

    pub fn visible_area(&self, output_width: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(
            self.location(output_width),
            (self.window.size.w, self.ui.get_panel_height().ceil() as i32).into(),
        )
    }

    pub fn is_closed(&self) -> bool {
        !self.ui.get_open() && !self.window.is_animating()
    }

    pub fn key(&self, key: &KeysymHandle<'_>, pressed: bool) {
        if !self.ui.get_open() {
            return;
        }

        if key.modified_sym().raw() == keysyms::KEY_Escape {
            if pressed {
                self.actions.borrow_mut().push(SettingsAction::Close);
            }
        } else {
            self.window.text_key(key, pressed);
        }
    }

    fn thumbnail(&self, path: &PathBuf, pixels: &RgbaImage) -> Image {
        self.thumbnails
            .borrow_mut()
            .entry(path.clone())
            .or_insert_with(|| {
                Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                    pixels.as_raw(),
                    pixels.width(),
                    pixels.height(),
                ))
            })
            .clone()
    }

    pub fn update(&self, shell: &Shell, outputs: &[OutputDetails], supports_night_light: bool) {
        let ui = &self.ui;
        let system = &shell.system;
        let status = &shell.status;

        let query = ui.get_query().to_lowercase();
        update_model(
            &self.topics,
            search::matching_topics(query.trim())
                .into_iter()
                .map(|topic| TopicItem {
                    page: PAGES[topic.page],
                    title: topic.title.into(),
                    subtitle: topic.subtitle.into(),
                })
                .collect(),
        );

        ui.set_error(system.error.as_deref().unwrap_or_default().into());
        ui.set_wifi_enabled(system.wifi_enabled);
        ui.set_bluetooth_enabled(system.bluetooth_enabled);
        ui.set_brightness(
            system
                .brightness
                .map_or(-1.0, |percent| percent as f32 / 100.0),
        );
        ui.set_dark(shell.dark);
        ui.set_night_light(shell.night_light);
        ui.set_night_light_available(supports_night_light);
        ui.set_volume(
            status
                .sound
                .map_or(0.0, |sound| sound.percent as f32 / 100.0),
        );
        ui.set_muted(status.sound.is_some_and(|sound| sound.muted));
        ui.set_mic_muted(system.mic_muted);
        ui.set_do_not_disturb(shell.do_not_disturb);
        ui.set_notification_count(shell.notifications.len() as i32);
        ui.set_battery_percent(status.battery.map_or(-1, |battery| battery.percent.into()));
        ui.set_battery_state(
            match status.battery {
                Some(battery) if battery.charging => "Plugged in",
                _ => "On battery",
            }
            .into(),
        );
        ui.set_power_saver(system.power_saver == Some(true));
        ui.set_power_saver_available(system.power_saver.is_some());
        ui.set_stay_awake(shell.stay_awake);

        update_model(
            &self.outputs,
            outputs
                .iter()
                .map(|output| OutputItem {
                    name: output.name.as_str().into(),
                    resolution: output.resolution.as_str().into(),
                    scale: output.scale as f32,
                })
                .collect(),
        );
        update_model(
            &self.networks,
            system
                .networks
                .iter()
                .map(|network| NetworkItem {
                    ssid: network.ssid.as_str().into(),
                    bars: status::wifi_bars(network.strength).into(),
                    secured: network.secured,
                    active: network.active,
                    known: network.known,
                    connecting: network.connecting,
                })
                .collect(),
        );
        update_model(
            &self.bluetooth_devices,
            system
                .bluetooth_devices
                .iter()
                .map(|device| BluetoothItem {
                    id: device.id.as_str().into(),
                    name: device.name.as_str().into(),
                    icon: device.icon.as_str().into(),
                    connected: device.connected,
                    paired: device.paired,
                    connecting: device.connecting,
                    battery: device.battery.map_or(-1, i32::from),
                })
                .collect(),
        );
        update_model(
            &self.sinks,
            system
                .sinks
                .iter()
                .map(|sink| SinkItem {
                    name: sink.name.as_str().into(),
                    description: sink.description.as_str().into(),
                    active: sink.active,
                })
                .collect(),
        );

        let current = shell.settings.wallpaper.as_ref();
        update_model(
            &self.wallpapers,
            shell
                .wallpapers
                .iter()
                .map(|(path, pixels)| WallpaperItem {
                    path: path.to_string_lossy().as_ref().into(),
                    thumbnail: self.thumbnail(path, pixels),
                    active: Some(path) == current,
                })
                .collect(),
        );
    }
}
