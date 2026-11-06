// SPDX-License-Identifier: GPL-3.0-or-later

//! The settings app of the Aether desktop.
//!
//! It's a normal Wayland app. What it shows and changes belongs to the compositor's shell,
//! which it asks over a socket.

mod about;

use std::{
    cell::RefCell,
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
    sync::mpsc,
    thread,
    time::Duration,
};

use aether_common::{Colors, Page, Request, Rgb, SETTINGS_APP_ID, Snapshot, matching_topics};
use anyhow::Context;
use image::{ImageReader, RgbaImage, imageops::FilterType};
use slint::{
    Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString,
    VecModel,
};

slint::include_modules!();

/// How often the shell is asked how things are.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The size of wallpapers when they're being chosen from.
const THUMBNAIL_WIDTH: u32 = 480;
const THUMBNAIL_HEIGHT: u32 = 270;

/// The connection to the shell.
struct Shell {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Shell {
    fn connect() -> anyhow::Result<Self> {
        let display = env::var("WAYLAND_DISPLAY").context("WAYLAND_DISPLAY isn't set")?;
        let path = aether_common::socket_path(&display).context("XDG_RUNTIME_DIR isn't set")?;
        let writer = UnixStream::connect(&path)
            .with_context(|| format!("can't reach the shell at {}", path.display()))?;
        let reader = BufReader::new(writer.try_clone()?);
        Ok(Self { writer, reader })
    }

    /// Sends `request` and gives back the line that answers it.
    fn ask(&mut self, request: &Request) -> anyhow::Result<String> {
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;

        let mut answer = String::new();
        if self.reader.read_line(&mut answer)? == 0 {
            anyhow::bail!("the shell went away");
        }
        Ok(answer)
    }

    fn snapshot(&mut self) -> anyhow::Result<(String, Snapshot)> {
        let line = self.ask(&Request::Snapshot)?;
        let snapshot = serde_json::from_str(&line)?;
        Ok((line, snapshot))
    }
}

/// The models the UI's lists show, and what's needed to keep them up to date.
struct Lists {
    networks: Rc<VecModel<NetworkItem>>,
    bluetooth_devices: Rc<VecModel<BluetoothItem>>,
    sinks: Rc<VecModel<SinkItem>>,
    outputs: Rc<VecModel<OutputItem>>,
    wallpapers: Rc<VecModel<WallpaperItem>>,
    /// The ones found so far, sorted by path.
    thumbnails: RefCell<Vec<(PathBuf, Image)>>,
    current_wallpaper: RefCell<Option<PathBuf>>,
    /// Whether wallpapers were looked for.
    wallpapers_requested: RefCell<bool>,
}

impl Lists {
    fn show_wallpapers(&self) {
        let current = self.current_wallpaper.borrow();
        update_model(
            &self.wallpapers,
            self.thumbnails
                .borrow()
                .iter()
                .map(|(path, thumbnail)| WallpaperItem {
                    path: path.to_string_lossy().as_ref().into(),
                    thumbnail: thumbnail.clone(),
                    active: Some(path) == current.as_ref(),
                })
                .collect(),
        );
    }
}

fn update_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (index, row) in rows.into_iter().enumerate() {
        if model.row_data(index).as_ref() != Some(&row) {
            model.set_row_data(index, row);
        }
    }
}

fn to_ui_page(page: Page) -> SettingsPage {
    match page {
        Page::Network => SettingsPage::Network,
        Page::Devices => SettingsPage::Devices,
        Page::Display => SettingsPage::Display,
        Page::Style => SettingsPage::Style,
        Page::Sound => SettingsPage::Sound,
        Page::Notifications => SettingsPage::Notifications,
        Page::Battery => SettingsPage::Battery,
        Page::About => SettingsPage::About,
    }
}

fn from_ui_page(page: SettingsPage) -> Page {
    match page {
        SettingsPage::Network => Page::Network,
        SettingsPage::Devices => Page::Devices,
        SettingsPage::Display => Page::Display,
        SettingsPage::Style => Page::Style,
        SettingsPage::Sound => Page::Sound,
        SettingsPage::Notifications => Page::Notifications,
        SettingsPage::Battery => Page::Battery,
        SettingsPage::About => Page::About,
    }
}

fn apply_colors(ui: &SettingsApp, colors: &Colors) {
    let color = |[red, green, blue]: Rgb| Color::from_rgb_u8(red, green, blue);
    let theme = Theme::get(ui);
    theme.set_surface(color(colors.surface));
    theme.set_primary(color(colors.primary));
    theme.set_on_primary(color(colors.on_primary));
    theme.set_accent(color(colors.accent));
    theme.set_on_surface(color(colors.on_surface));
    theme.set_on_surface_variant(color(colors.on_surface_variant));
    theme.set_error(color(colors.error));
}

fn show_topics(query: &str, topics: &VecModel<TopicItem>) {
    update_model(
        topics,
        matching_topics(query.trim().to_lowercase().as_str())
            .into_iter()
            .map(|topic| TopicItem {
                page: to_ui_page(topic.page),
                title: topic.title.into(),
                subtitle: topic.subtitle.into(),
            })
            .collect(),
    );
}

/// A small version of the image at `path` for choosing it.
fn thumbnail(path: &Path) -> Option<RgbaImage> {
    let image = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    Some(
        image
            .resize_to_fill(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, FilterType::Triangle)
            .into_rgba8(),
    )
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp"
            )
        })
}

/// Makes small versions of the wallpapers to choose from, and shows them as they get done.
fn find_wallpapers(ui: slint::Weak<SettingsApp>, lists: &Lists) {
    if lists.wallpapers_requested.replace(true) {
        return;
    }

    let mut dirs = lists
        .current_wallpaper
        .borrow()
        .iter()
        .filter_map(|path| Some(path.parent()?.to_owned()))
        .collect::<Vec<_>>();
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Pictures/Wallpapers"));
    }
    dirs.sort();
    dirs.dedup();

    thread::spawn(move || {
        let paths = dirs
            .iter()
            .filter_map(|dir| fs::read_dir(dir).ok())
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_image(path));

        for path in paths {
            let Some(pixels) = thumbnail(&path) else {
                continue;
            };
            let added = ui.upgrade_in_event_loop(move |_| {
                LISTS.with_borrow(|lists| {
                    let Some(lists) = lists else {
                        return;
                    };
                    let image =
                        Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                            pixels.as_raw(),
                            pixels.width(),
                            pixels.height(),
                        ));
                    let mut thumbnails = lists.thumbnails.borrow_mut();
                    let place = thumbnails.partition_point(|(other, _)| *other < path);
                    thumbnails.insert(place, (path, image));
                    drop(thumbnails);
                    lists.show_wallpapers();
                });
            });
            if added.is_err() {
                return;
            }
        }
    });
}

thread_local! {
    static LISTS: RefCell<Option<Rc<Lists>>> = const { RefCell::new(None) };
}

fn open_page(ui: &SettingsApp, lists: &Lists, requests: &mpsc::Sender<Request>, page: Page) {
    ui.set_page(to_ui_page(page));
    ui.set_detail_open(true);
    let _ = requests.send(Request::Showing(page));
    if page == Page::Style {
        find_wallpapers(ui.as_weak(), lists);
    }
}

fn apply_snapshot(
    ui: &SettingsApp,
    lists: &Lists,
    requests: &mpsc::Sender<Request>,
    snapshot: Snapshot,
) {
    apply_colors(ui, &snapshot.colors);

    ui.set_error(snapshot.error.unwrap_or_default().into());
    ui.set_wifi_enabled(snapshot.wifi_enabled);
    ui.set_bluetooth_enabled(snapshot.bluetooth_enabled);
    ui.set_brightness(
        snapshot
            .brightness
            .map_or(-1.0, |percent| percent as f32 / 100.0),
    );
    ui.set_dark(snapshot.dark);
    ui.set_night_light(snapshot.night_light);
    ui.set_night_light_available(snapshot.night_light_available);
    ui.set_volume(
        snapshot
            .volume
            .map_or(0.0, |percent| percent as f32 / 100.0),
    );
    ui.set_muted(snapshot.muted);
    ui.set_mic_muted(snapshot.mic_muted);
    ui.set_do_not_disturb(snapshot.do_not_disturb);
    ui.set_notification_count(snapshot.notification_count as i32);
    ui.set_battery_percent(
        snapshot
            .battery
            .map_or(-1, |battery| battery.percent.into()),
    );
    ui.set_battery_state(
        match snapshot.battery {
            Some(battery) if battery.charging => "Plugged in",
            _ => "On battery",
        }
        .into(),
    );
    ui.set_power_saver(snapshot.power_saver == Some(true));
    ui.set_power_saver_available(snapshot.power_saver.is_some());
    ui.set_stay_awake(snapshot.stay_awake);
    ui.set_rounded_corners(snapshot.rounded_corners);
    ui.set_blur(snapshot.blur);

    update_model(
        &lists.outputs,
        snapshot
            .outputs
            .iter()
            .map(|output| OutputItem {
                name: output.name.as_str().into(),
                resolution: output.resolution.as_str().into(),
                scale: output.scale as f32,
            })
            .collect(),
    );
    update_model(
        &lists.networks,
        snapshot
            .networks
            .iter()
            .map(|network| NetworkItem {
                ssid: network.ssid.as_str().into(),
                bars: network.bars.into(),
                secured: network.secured,
                active: network.active,
                known: network.known,
                connecting: network.connecting,
            })
            .collect(),
    );
    update_model(
        &lists.bluetooth_devices,
        snapshot
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
        &lists.sinks,
        snapshot
            .sinks
            .iter()
            .map(|sink| SinkItem {
                name: sink.name.as_str().into(),
                description: sink.description.as_str().into(),
                active: sink.active,
            })
            .collect(),
    );

    *lists.current_wallpaper.borrow_mut() = snapshot.wallpaper;
    lists.show_wallpapers();

    if let Some(page) = snapshot.page {
        open_page(ui, lists, requests, page);
    }
}

/// Keeps the UI up with the shell, and sends it what the UI asks for.
fn talk_to_shell(
    mut shell: Shell,
    requests: mpsc::Receiver<Request>,
    ui: slint::Weak<SettingsApp>,
) {
    let mut last_line = String::new();

    loop {
        match requests.recv_timeout(POLL_INTERVAL) {
            Ok(request) => {
                if shell.ask(&request).is_err() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        let Ok((line, snapshot)) = shell.snapshot() else {
            break;
        };
        if line == last_line {
            continue;
        }
        last_line = line;

        let applied = ui.upgrade_in_event_loop(move |ui| {
            REQUESTS.with_borrow(|requests| {
                LISTS.with_borrow(|lists| {
                    if let (Some(requests), Some(lists)) = (requests, lists) {
                        apply_snapshot(&ui, lists, requests, snapshot);
                    }
                });
            });
        });
        if applied.is_err() {
            return;
        }
    }

    // There's nothing to show without the shell.
    let _ = slint::invoke_from_event_loop(|| {
        let _ = slint::quit_event_loop();
    });
}

thread_local! {
    static REQUESTS: RefCell<Option<mpsc::Sender<Request>>> = const { RefCell::new(None) };
}

fn run() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let mut page = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--page" => {
                let name = args.next().context("--page needs a page")?;
                page = Some(Page::from_name(&name).with_context(|| format!("no page {name}"))?);
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }

    // The window only appears when its colors are known.
    let mut shell = Shell::connect()?;
    let (_, snapshot) = shell.snapshot()?;

    let ui = SettingsApp::new()?;
    // This needs the backend that creating the UI chooses, and comes before the window shows.
    slint::set_xdg_app_id(SETTINGS_APP_ID)?;

    let (requests, request_receiver) = mpsc::channel();

    let topics = Rc::new(VecModel::default());
    ui.set_topics(ModelRc::from(topics.clone()));
    let lists = Rc::new(Lists {
        networks: Rc::new(VecModel::default()),
        bluetooth_devices: Rc::new(VecModel::default()),
        sinks: Rc::new(VecModel::default()),
        outputs: Rc::new(VecModel::default()),
        wallpapers: Rc::new(VecModel::default()),
        thumbnails: RefCell::default(),
        current_wallpaper: RefCell::default(),
        wallpapers_requested: RefCell::default(),
    });
    ui.set_networks(ModelRc::from(lists.networks.clone()));
    ui.set_bluetooth_devices(ModelRc::from(lists.bluetooth_devices.clone()));
    ui.set_sinks(ModelRc::from(lists.sinks.clone()));
    ui.set_outputs(ModelRc::from(lists.outputs.clone()));
    ui.set_wallpapers(ModelRc::from(lists.wallpapers.clone()));
    ui.set_about(ModelRc::from(Rc::new(VecModel::from(
        about::system_info()
            .into_iter()
            .map(|(title, value)| InfoItem {
                title: title.into(),
                value: value.into(),
            })
            .collect::<Vec<_>>(),
    ))));
    LISTS.set(Some(lists.clone()));
    REQUESTS.set(Some(requests.clone()));

    let send = |request: Request| {
        let requests = requests.clone();
        move || {
            let _ = requests.send(request.clone());
        }
    };
    let send_text = |make: fn(String) -> Request| {
        let requests = requests.clone();
        move |text: SharedString| {
            let _ = requests.send(make(text.into()));
        }
    };
    let send_percent = |make: fn(u8) -> Request| {
        let requests = requests.clone();
        move |value: f32| {
            let _ = requests.send(make((value * 100.0).round() as u8));
        }
    };

    ui.on_closed(|| {
        let _ = slint::quit_event_loop();
    });
    ui.on_wifi_toggled(send(Request::ToggleWifi));
    ui.on_bluetooth_toggled(send(Request::ToggleBluetooth));
    ui.on_dark_toggled(send(Request::ToggleDark));
    ui.on_night_light_toggled(send(Request::ToggleNightLight));
    ui.on_mute_toggled(send(Request::ToggleMute));
    ui.on_mic_toggled(send(Request::ToggleMicrophone));
    ui.on_do_not_disturb_toggled(send(Request::ToggleDoNotDisturb));
    ui.on_notifications_cleared(send(Request::ClearNotifications));
    ui.on_power_saver_toggled(send(Request::TogglePowerSaver));
    ui.on_stay_awake_toggled(send(Request::ToggleStayAwake));
    ui.on_rounded_corners_toggled(send(Request::ToggleRoundedCorners));
    ui.on_blur_toggled(send(Request::ToggleBlur));
    ui.on_network_clicked(send_text(|ssid| Request::ClickNetwork { ssid }));
    ui.on_network_forgotten(send_text(|ssid| Request::ForgetWifi { ssid }));
    ui.on_bluetooth_clicked(send_text(|id| Request::ClickBluetooth { id }));
    ui.on_sink_chosen(send_text(Request::SetSink));
    ui.on_brightness_changed(send_percent(Request::SetBrightness));
    ui.on_volume_changed(send_percent(Request::SetVolume));
    ui.on_password_entered({
        let requests = requests.clone();
        move |ssid, password| {
            let _ = requests.send(Request::ConnectWifi {
                ssid: ssid.into(),
                password: password.into(),
            });
        }
    });
    ui.on_scale_chosen({
        let requests = requests.clone();
        move |output, scale| {
            let _ = requests.send(Request::SetScale {
                output: output.into(),
                scale,
            });
        }
    });
    ui.on_wallpaper_chosen({
        let requests = requests.clone();
        move |path| {
            let _ = requests.send(Request::SetWallpaper(path.as_str().into()));
        }
    });
    ui.on_query_changed({
        let topics = topics.clone();
        move |query| show_topics(&query, &topics)
    });
    ui.on_page_chosen({
        let ui = ui.as_weak();
        let lists = lists.clone();
        let requests = requests.clone();
        move |page| {
            if let Some(ui) = ui.upgrade() {
                open_page(&ui, &lists, &requests, from_ui_page(page));
            }
        }
    });

    show_topics("", &topics);
    apply_snapshot(&ui, &lists, &requests, snapshot);
    open_page(&ui, &lists, &requests, page.unwrap_or(Page::Network));
    // Started by itself, it starts from the list when there's only room for one thing.
    ui.set_detail_open(page.is_some());
    ui.invoke_focus_search();

    thread::spawn({
        let ui = ui.as_weak();
        move || talk_to_shell(shell, request_receiver, ui)
    });

    ui.run()?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("aether-settings: {err:#}");
            ExitCode::FAILURE
        }
    }
}
