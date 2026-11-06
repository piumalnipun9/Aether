// SPDX-License-Identifier: GPL-3.0-or-later

//! The desktop shell, drawn by the compositor itself.
//!
//! The UI is written in Slint. Slint's software renderer draws each output's bar into
//! a [`MemoryRenderBuffer`], which is rendered like any other element. Quick settings
//! is drawn the same way, on top of a blurred copy of what's behind it. So is the
//! wallpaper, which the shell's colors come from.
//!
//! The settings are read from `shell.toml` in the config directory:
//!
//! ```toml
//! wallpaper = "~/Pictures/wallpaper.jpg"
//! ```
//!
//! The quick settings tiles are kept in `quick-settings.toml` next to it, which quick
//! settings writes itself when they're edited.

mod about;
mod apps;
mod calendar;
mod clock;
mod launcher;
mod notifications;
mod quick_settings;
mod search;
mod settings_app;
mod status;
mod system;
mod wallpaper;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    convert::Infallible,
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Stdio,
    rc::Rc,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use image::RgbaImage;
use serde::Deserialize;
use slint::{
    Color, ComponentHandle, Global, LogicalPosition, Model, ModelRc, PhysicalSize, PlatformError,
    SharedString, VecModel,
    platform::{
        Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType},
    },
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::element::{
            Kind,
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
        },
    },
    desktop::{Space, layer_map_for_output},
    input::keyboard::{KeysymHandle, ModifiersState, keysyms},
    output::{Output, WeakOutput},
    reexports::calloop::channel::{self, Event, Sender},
    render_elements,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::shell::wlr_layer,
};
use tracing::{error, warn};

use self::{
    apps::App,
    clock::{CLOCK_MAX_HEIGHT, CLOCK_WIDTH, ClockAction, Toast, ToastAction},
    launcher::{LauncherAction, Target},
    notifications::{CloseReason, Notification},
    quick_settings::{Place, TileKind, Tiles},
    settings_app::{OutputDetails, PAGES, SettingsAction},
    status::{Radio, Sound, Status},
    system::{Command, System, Watching},
    wallpaper::{Palette, Palettes, Rgb},
};
pub use self::{
    clock::{OutputClock, OutputToasts},
    launcher::OutputLauncher,
    settings_app::OutputSettings,
};
use crate::{
    api::workspace::switch_to,
    backend::Backend,
    output::OutputState,
    render::{
        AsGlesRenderer, OutputRenderElement, PRenderer,
        util::{
            blur::{Blur, DOWNSCALE},
            surface::{WlSurfaceTextureRenderElement, texture_render_elements_from_surface_tree},
        },
    },
    state::{Pinnacle, State, WithState},
    window::WindowElement,
};

slint::include_modules!();

/// The height of the top bar in logical pixels.
pub const BAR_HEIGHT: i32 = 38;

/// The width of quick settings in logical pixels.
const PANEL_WIDTH: i32 = 398;

/// The most space quick settings can take up.
const PANEL_MAX_HEIGHT: i32 = 680;

/// The space between quick settings and the edges of the screen and bar.
const PANEL_MARGIN: i32 = 8;

/// How many notifications are kept.
const MAX_NOTIFICATIONS: usize = 50;

/// What Night Light multiplies the color channels by.
const NIGHT_LIGHT: [f64; 3] = [1.0, 0.8, 0.58];

/// How long what global search finds is good for.
const SEARCH_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// The file the shell's settings are in, in the config directory.
const SETTINGS_FILE: &str = "shell.toml";

/// How often the clock is reformatted.
const CLOCK_INTERVAL: Duration = Duration::from_secs(1);

/// Linux's `BTN_LEFT`.
pub const BTN_LEFT: u32 = 0x110;
/// Linux's `BTN_RIGHT`.
pub const BTN_RIGHT: u32 = 0x111;

thread_local! {
    /// The window most recently created by [`ShellPlatform`].
    static NEW_WINDOW: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) };
}

/// Runs Slint inside the compositor's event loop, without a windowing system.
struct ShellPlatform;

impl Platform for ShellPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        // Shell windows are translucent and get drawn on top of what was there, so they
        // have to start from an empty buffer every time.
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        NEW_WINDOW.set(Some(window.clone()));
        Ok(window)
    }
}

/// Makes Slint use [`ShellPlatform`], which has to happen before creating any UI.
fn set_platform() {
    // Fails only because it was already set, which is what we want.
    let _ = slint::platform::set_platform(Box::new(ShellPlatform));
}

/// Shell state that isn't specific to an output.
#[derive(Debug, Default)]
pub struct Shell {
    /// Set when Slint can't run, so that it isn't retried on every event loop cycle.
    disabled: bool,
    clock_time: String,
    clock_date: String,
    date: String,
    clock_updated: Option<Instant>,
    status: Status,
    settings: Settings,
    system: System,
    /// Whether the theme is dark.
    dark: bool,
    palette: Option<Palettes>,
    /// Whether a palette was already requested from the wallpaper.
    palette_requested: bool,
    /// Where other threads send their results. Set once the shell has started.
    sender: Option<Sender<Message>>,
    /// What the worker thread takes orders through.
    commands: Option<mpsc::Sender<Command>>,
    tiles: Tiles,
    /// Which panel is open, and on which output.
    open_panel: Option<(WeakOutput, ShellSurface)>,
    /// From oldest to newest.
    notifications: Vec<Notification>,
    /// The ones popping up, from oldest to newest.
    toasts: Vec<Toast>,
    do_not_disturb: bool,
    /// How many months away the calendar is from this one.
    calendar_offset: i32,
    /// The installed apps, sorted by name.
    apps: Vec<App>,
    /// Goes up whenever `apps` changes.
    apps_generation: u32,
    /// When the apps were last looked for.
    apps_scanned: Option<Instant>,
    /// How often each app was opened, by its id.
    launch_counts: HashMap<String, u32>,
    /// The files search looks through.
    files: Vec<PathBuf>,
    /// The wallpapers to choose from, with small versions of them.
    wallpapers: Vec<(PathBuf, RgbaImage)>,
    /// Whether wallpapers were looked for.
    wallpapers_requested: bool,
    /// Whether Super was pressed with nothing else since, which makes it a tap so far.
    super_tapping: bool,
    /// Whether the launcher was asked for from the keyboard.
    launcher_requested: bool,
    /// Whether the tiles are being rearranged.
    editing: bool,
    /// The tile being edited.
    selected: Option<TileKind>,
    /// Whether the session is kept from going idle.
    pub stay_awake: bool,
    night_light: bool,
    /// The outputs Night Light has been applied to.
    night_light_outputs: Vec<WeakOutput>,
}

/// The contents of `shell.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    wallpaper: Option<PathBuf>,
}

impl Settings {
    /// Reads the settings from `config_dir`. Without a valid file, these are the defaults.
    fn read(config_dir: &Path) -> Self {
        let path = config_dir.join(SETTINGS_FILE);
        let Ok(contents) = fs::read_to_string(&path) else {
            return Self::default();
        };

        let mut settings = toml::from_str::<Self>(&contents).unwrap_or_else(|err| {
            error!("Failed to parse {}: {err}", path.display());
            Self::default()
        });

        settings.wallpaper = settings
            .wallpaper
            .map(|wallpaper| config_dir.join(shellexpand::path::tilde(&wallpaper)));

        settings
    }
}

impl Settings {
    /// Saves the wallpaper, leaving the rest of the file as it is.
    fn write_wallpaper(config_dir: &Path, wallpaper: &str) {
        let path = config_dir.join(SETTINGS_FILE);
        let contents = fs::read_to_string(&path).unwrap_or_default();

        if let Err(err) = fs::write(&path, wallpaper::with_wallpaper(&contents, wallpaper)) {
            error!("Failed to write {}: {err}", path.display());
        }
    }
}

/// Where the launcher keeps how often apps were opened.
fn launch_counts_file() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state")))?;
    Some(dir.join("pinnacle/launcher.toml"))
}

fn read_launch_counts() -> HashMap<String, u32> {
    launch_counts_file()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|contents| toml::from_str(&contents).ok())
        .unwrap_or_default()
}

fn write_launch_counts(counts: &HashMap<String, u32>) {
    let Some(path) = launch_counts_file() else {
        return;
    };
    let written = toml::to_string(counts)
        .map_err(anyhow::Error::from)
        .and_then(|contents| {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            Ok(fs::write(&path, contents)?)
        });
    if let Err(err) = written {
        warn!("Failed to write {}: {err}", path.display());
    }
}

/// Results from the shell's other threads.
#[derive(Debug)]
enum Message {
    Status(Status),
    System(Box<System>),
    Notification(notifications::Event),
    Apps(Vec<App>),
    Files(Vec<PathBuf>),
    Thumbnail(PathBuf, RgbaImage),
    Palette(Palettes),
    Wallpaper {
        output: String,
        size: Size<i32, Physical>,
        image: image::RgbaImage,
    },
}

/// An output's wallpaper.
#[derive(Debug)]
pub struct OutputWallpaper {
    /// The size of the output when this wallpaper was asked for.
    requested: (Size<i32, Physical>, Size<i32, Logical>),
    /// Stays empty until the wallpaper has loaded. This comes with its size in pixels.
    buffer: Option<(MemoryRenderBuffer, Size<i32, Physical>)>,
}

impl OutputWallpaper {
    /// The wallpaper as a render element covering the output.
    pub fn render_element<R: PRenderer>(
        &self,
        renderer: &mut R,
    ) -> Option<MemoryRenderBufferRenderElement<R>> {
        let (buffer, pixel_size) = self.buffer.as_ref()?;
        memory_element(
            renderer,
            Point::default(),
            buffer,
            *pixel_size,
            self.requested.1,
        )
    }
}

/// `pixel_size` is the size of `buffer`, and `size` is how large to show it. Without the
/// former being spelled out, only the part of the buffer that's `size` large would show.
fn memory_element<R: PRenderer>(
    renderer: &mut R,
    location: Point<i32, Physical>,
    buffer: &MemoryRenderBuffer,
    pixel_size: Size<i32, Physical>,
    size: Size<i32, Logical>,
) -> Option<MemoryRenderBufferRenderElement<R>> {
    let source = Size::<f64, Logical>::from((pixel_size.w as f64, pixel_size.h as f64));

    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        location.to_f64(),
        buffer,
        None,
        Some(Rectangle::from_size(source)),
        Some(size),
        Kind::Unspecified,
    )
    .inspect_err(|err| error!("Failed to import a shell buffer: {err:?}"))
    .ok()
}

impl Shell {
    fn update_clock(&mut self) {
        if self
            .clock_updated
            .is_none_or(|updated| updated.elapsed() >= CLOCK_INTERVAL)
        {
            let now = chrono::Local::now();
            self.date = now.format("%a, %b %-d").to_string();
            self.clock_time = now.format("%H:%M").to_string();
            self.clock_date = now.format("%a %-d").to_string();
            self.clock_updated = Some(Instant::now());
        }
    }
}

/// What a bar shows.
struct BarContent {
    workspaces: Vec<WorkspaceItem>,
    active_index: i32,
    title: String,
    clock_time: String,
    clock_date: String,
    has_notifications: bool,
    do_not_disturb: bool,
    status: Status,
}

fn apply_palette(theme: Theme<'_>, palette: &Palette) {
    let color = |[r, g, b]: Rgb| Color::from_rgb_u8(r, g, b);
    theme.set_surface(color(palette.surface));
    theme.set_primary(color(palette.primary));
    theme.set_on_primary(color(palette.on_primary));
    theme.set_accent(color(palette.accent));
    theme.set_on_surface(color(palette.on_surface));
    theme.set_on_surface_variant(color(palette.on_surface_variant));
    theme.set_error(color(palette.error));
}

/// Which of an output's shell windows something is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellSurface {
    Bar,
    QuickSettings,
    Clock,
    Toasts,
    Launcher,
    Settings,
}

/// A Slint window drawn into a buffer.
pub struct ShellWindow {
    window: Rc<MinimalSoftwareWindow>,
    buffer: MemoryRenderBuffer,
    pointer: Cell<LogicalPosition>,
    pressed: Cell<bool>,
    size: Size<i32, Logical>,
    scale: f64,
    pixel_size: Size<i32, Physical>,
}

impl ShellWindow {
    fn new(
        ui: &impl ComponentHandle,
        size: Size<i32, Logical>,
        scale: f64,
    ) -> Result<Self, PlatformError> {
        ui.show()?;
        let window = NEW_WINDOW
            .take()
            .ok_or_else(|| PlatformError::Other("Slint didn't create a window".into()))?;

        let pixel_size = Size::<i32, smithay::utils::Buffer>::from((
            (size.w as f64 * scale).round() as i32,
            (size.h as f64 * scale).round() as i32,
        ));

        ui.window().dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale as f32,
        });
        window.set_size(PhysicalSize::new(pixel_size.w as u32, pixel_size.h as u32));

        Ok(Self {
            window,
            buffer: MemoryRenderBuffer::new(
                Fourcc::Abgr8888,
                pixel_size,
                1,
                Transform::Normal,
                None,
            ),
            pointer: Cell::new(LogicalPosition::default()),
            pressed: Cell::new(false),
            size,
            scale,
            pixel_size: (pixel_size.w, pixel_size.h).into(),
        })
    }

    fn matches(&self, size: Size<i32, Logical>, scale: f64) -> bool {
        self.size == size && self.scale == scale
    }

    fn matches_scale(&self, scale: f64) -> bool {
        self.scale == scale
    }

    /// Whether something in the window needs to be redrawn soon.
    pub fn is_animating(&self) -> bool {
        // Slint's timers only run when the event loop does, and one may be
        // waiting for a button to be held for long enough.
        self.window.window().has_active_animations() || self.pressed.get()
    }

    /// Redraws what changed. Returns whether anything did.
    fn draw(&mut self) -> bool {
        let Self {
            window,
            buffer,
            pixel_size,
            ..
        } = self;

        let mut drawn = false;
        let result = buffer.render().draw(|mem| {
            let mut damage = Vec::new();
            drawn = window.draw_if_needed(|renderer| {
                mem.fill(0);
                let pixels: &mut [PremultipliedRgbaColor] = bytemuck::cast_slice_mut(mem);
                let region = renderer.render(pixels, pixel_size.w as usize);
                damage.extend(region.iter().map(|(loc, size)| {
                    Rectangle::new(
                        (loc.x, loc.y).into(),
                        (size.width as i32, size.height as i32).into(),
                    )
                }));
            });
            Ok::<_, Infallible>(damage)
        });
        let Ok(()) = result;

        drawn
    }

    fn render_element<R: PRenderer>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
    ) -> Option<MemoryRenderBufferRenderElement<R>> {
        memory_element(renderer, location, &self.buffer, self.pixel_size, self.size)
    }

    pub fn pointer_moved(&self, location: Point<f64, Logical>) {
        let position = LogicalPosition::new(location.x as f32, location.y as f32);
        self.pointer.set(position);
        self.window
            .window()
            .dispatch_event(WindowEvent::PointerMoved { position });
    }

    pub fn pointer_button(&self, button: u32, pressed: bool) {
        let position = self.pointer.get();
        let button = match button {
            BTN_LEFT => PointerEventButton::Left,
            BTN_RIGHT => PointerEventButton::Right,
            _ => PointerEventButton::Other,
        };
        self.pressed.set(pressed);
        self.window.window().dispatch_event(if pressed {
            WindowEvent::PointerPressed { position, button }
        } else {
            WindowEvent::PointerReleased { position, button }
        });
    }

    pub fn pointer_scrolled(&self, horizontal: f64, vertical: f64) {
        self.window
            .window()
            .dispatch_event(WindowEvent::PointerScrolled {
                position: self.pointer.get(),
                delta_x: -horizontal as f32,
                delta_y: -vertical as f32,
            });
    }

    pub fn pointer_left(&self) {
        self.pressed.set(false);
        self.window
            .window()
            .dispatch_event(WindowEvent::PointerExited);
    }

    /// Hands a key to the text field that has focus.
    fn text_key(&self, key: &KeysymHandle<'_>, pressed: bool) {
        let text: Option<SharedString> = match key.modified_sym().raw() {
            keysyms::KEY_BackSpace => Some(Key::Backspace.into()),
            keysyms::KEY_Delete => Some(Key::Delete.into()),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => Some(Key::Return.into()),
            keysyms::KEY_Left => Some(Key::LeftArrow.into()),
            keysyms::KEY_Right => Some(Key::RightArrow.into()),
            keysyms::KEY_Home => Some(Key::Home.into()),
            keysyms::KEY_End => Some(Key::End.into()),
            _ => key
                .modified_sym()
                .key_char()
                .filter(|c| !c.is_control())
                .map(|c| c.to_string().into()),
        };

        if let Some(text) = text {
            self.key(text, pressed);
        }
    }

    fn key(&self, text: SharedString, pressed: bool) {
        self.window.window().dispatch_event(if pressed {
            WindowEvent::KeyPressed { text }
        } else {
            WindowEvent::KeyReleased { text }
        });
    }
}

/// What was clicked on a bar.
enum BarAction {
    Workspace(usize),
    QuickSettings,
    Clock,
    Launcher,
}

/// An output's top bar.
pub struct OutputBar {
    ui: Bar,
    pub window: ShellWindow,
    workspaces: Rc<VecModel<WorkspaceItem>>,
    /// What was clicked since the last [`OutputBar::take_actions`].
    actions: Rc<RefCell<Vec<BarAction>>>,
}

impl std::fmt::Debug for OutputBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputBar")
            .field("size", &self.window.size)
            .field("scale", &self.window.scale)
            .finish_non_exhaustive()
    }
}

impl OutputBar {
    fn new(width: i32, scale: f64) -> Result<Self, PlatformError> {
        set_platform();
        let ui = Bar::new()?;
        let window = ShellWindow::new(&ui, (width, BAR_HEIGHT).into(), scale)?;

        let workspaces = Rc::new(VecModel::default());
        ui.set_workspaces(ModelRc::from(workspaces.clone()));

        let actions = Rc::new(RefCell::new(Vec::new()));
        ui.on_workspace_clicked({
            let actions = actions.clone();
            move |index| {
                actions
                    .borrow_mut()
                    .push(BarAction::Workspace(index as usize))
            }
        });
        ui.on_quick_settings_clicked({
            let actions = actions.clone();
            move || actions.borrow_mut().push(BarAction::QuickSettings)
        });
        ui.on_clock_clicked({
            let actions = actions.clone();
            move || actions.borrow_mut().push(BarAction::Clock)
        });
        ui.on_launcher_clicked({
            let actions = actions.clone();
            move || actions.borrow_mut().push(BarAction::Launcher)
        });

        Ok(Self {
            ui,
            window,
            workspaces,
            actions,
        })
    }

    fn take_actions(&self) -> Vec<BarAction> {
        std::mem::take(&mut self.actions.borrow_mut())
    }

    fn update(&self, content: BarContent) {
        if self.workspaces.row_count() == content.workspaces.len() {
            for (index, item) in content.workspaces.iter().enumerate() {
                if self.workspaces.row_data(index).as_ref() != Some(item) {
                    self.workspaces.set_row_data(index, item.clone());
                }
            }
        } else {
            self.workspaces.set_vec(content.workspaces);
        }

        // Slint ignores sets that don't change anything, so these don't cause redraws.
        self.ui.set_active_index(content.active_index);
        self.ui.set_window_title(SharedString::from(content.title));
        self.ui
            .set_clock_time(SharedString::from(content.clock_time));
        self.ui
            .set_clock_date(SharedString::from(content.clock_date));
        self.ui.set_has_notifications(content.has_notifications);
        self.ui.set_do_not_disturb(content.do_not_disturb);

        let radio_state = |radio| match radio {
            Radio::Missing => RadioState::Missing,
            Radio::Off => RadioState::Off,
            Radio::On => RadioState::On,
            Radio::Connected => RadioState::Connected,
        };

        let Status {
            wifi,
            wifi_bars,
            bluetooth,
            sound,
            battery,
            cpu,
            ram,
        } = content.status;
        self.ui.set_wifi_state(radio_state(wifi));
        self.ui.set_wifi_bars(wifi_bars.into());
        self.ui.set_bluetooth_state(radio_state(bluetooth));
        self.ui
            .set_volume_percent(sound.map_or(-1, |Sound { percent, .. }| percent.into()));
        self.ui
            .set_volume_muted(sound.is_some_and(|sound| sound.muted));
        self.ui
            .set_battery_percent(battery.map_or(-1, |battery| battery.percent.into()));
        self.ui
            .set_battery_charging(battery.is_some_and(|battery| battery.charging));
        self.ui.set_cpu_percent(cpu.into());
        self.ui.set_ram_percent(ram.into());
    }

    /// The bar as a render element at the top of the output.
    pub fn render_element<R: PRenderer>(
        &self,
        renderer: &mut R,
    ) -> Option<MemoryRenderBufferRenderElement<R>> {
        self.window.render_element(renderer, Point::default())
    }
}

/// What was done in quick settings.
enum PanelAction {
    Close,
    ToggleEditing,
    EditTile(TileKind),
    ToggleTile(TileKind),
    OpenTile(TileKind),
    ResizeTile(TileKind),
    RemoveSelectedTile,
    OpenPage(Page),
    OpenSettings(Page),
    ToggleWifi,
    ToggleBluetooth,
    ToggleMicrophone,
    LogOut,
    Run(Command),
}

/// Quick settings on an output.
pub struct OutputPanel {
    ui: QuickSettings,
    pub window: ShellWindow,
    blur: RefCell<Blur>,
    tiles: Rc<VecModel<TileItem>>,
    networks: Rc<VecModel<NetworkItem>>,
    bluetooth_devices: Rc<VecModel<BluetoothItem>>,
    sinks: Rc<VecModel<SinkItem>>,
    actions: Rc<RefCell<Vec<PanelAction>>>,
}

impl std::fmt::Debug for OutputPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputPanel").finish_non_exhaustive()
    }
}

/// Replaces a model's rows, keeping the ones that didn't change so they aren't redrawn.
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

impl OutputPanel {
    fn new(height: i32, scale: f64) -> Result<Self, PlatformError> {
        set_platform();
        let ui = QuickSettings::new()?;
        let window = ShellWindow::new(&ui, (PANEL_WIDTH, height).into(), scale)?;

        let tiles = Rc::new(VecModel::default());
        ui.set_tiles(ModelRc::from(tiles.clone()));
        let networks = Rc::new(VecModel::default());
        ui.set_networks(ModelRc::from(networks.clone()));
        let bluetooth_devices = Rc::new(VecModel::default());
        ui.set_bluetooth_devices(ModelRc::from(bluetooth_devices.clone()));
        let sinks = Rc::new(VecModel::default());
        ui.set_sinks(ModelRc::from(sinks.clone()));

        let actions = Rc::new(RefCell::new(Vec::new()));
        let queue = |make: fn() -> PanelAction| {
            let actions = actions.clone();
            move || actions.borrow_mut().push(make())
        };
        let tile = |make: fn(TileKind) -> PanelAction| {
            let actions = actions.clone();
            move |kind: i32| {
                let kind = TileKind::ALL[kind as usize];
                actions.borrow_mut().push(make(kind));
            }
        };

        ui.on_edit_toggled(queue(|| PanelAction::ToggleEditing));
        ui.on_tile_clicked(tile(PanelAction::EditTile));
        ui.on_tile_toggled(tile(PanelAction::ToggleTile));
        ui.on_tile_details(tile(PanelAction::OpenTile));
        ui.on_tile_resized(tile(PanelAction::ResizeTile));
        ui.on_selection_removed(queue(|| PanelAction::RemoveSelectedTile));
        ui.on_wifi_toggled(queue(|| PanelAction::ToggleWifi));
        ui.on_bluetooth_toggled(queue(|| PanelAction::ToggleBluetooth));
        ui.on_mic_toggled(queue(|| PanelAction::ToggleMicrophone));
        ui.on_log_out(queue(|| PanelAction::LogOut));
        ui.on_suspend(queue(|| PanelAction::Run(Command::Suspend)));
        ui.on_restart(queue(|| PanelAction::Run(Command::Restart)));
        ui.on_shut_down(queue(|| PanelAction::Run(Command::ShutDown)));

        let run = |make: fn(String) -> Command| {
            let actions = actions.clone();
            move |text: SharedString| {
                actions
                    .borrow_mut()
                    .push(PanelAction::Run(make(text.into())));
            }
        };
        ui.on_network_clicked(run(|ssid| Command::ClickNetwork { ssid }));
        ui.on_network_forgotten(run(|ssid| Command::ForgetWifi { ssid }));
        ui.on_bluetooth_clicked(run(|id| Command::ClickBluetooth { id }));
        ui.on_sink_chosen(run(Command::SetSink));

        let percent = |make: fn(u8) -> Command| {
            let actions = actions.clone();
            move |value: f32| {
                let percent = (value * 100.0).round() as u8;
                actions.borrow_mut().push(PanelAction::Run(make(percent)));
            }
        };
        ui.on_brightness_changed(percent(Command::SetBrightness));
        ui.on_volume_changed(percent(Command::SetVolume));

        ui.on_page_changed({
            let actions = actions.clone();
            move |page| actions.borrow_mut().push(PanelAction::OpenPage(page))
        });
        ui.on_settings_opened({
            let actions = actions.clone();
            move |page| actions.borrow_mut().push(PanelAction::OpenSettings(page))
        });
        ui.on_password_entered({
            let actions = actions.clone();
            move |ssid, password| {
                actions
                    .borrow_mut()
                    .push(PanelAction::Run(Command::ConnectWifi {
                        ssid: ssid.into(),
                        password: password.into(),
                    }));
            }
        });

        ui.set_open(true);

        Ok(Self {
            ui,
            window,
            blur: Default::default(),
            tiles,
            networks,
            bluetooth_devices,
            sinks,
            actions,
        })
    }

    fn take_actions(&self) -> Vec<PanelAction> {
        std::mem::take(&mut self.actions.borrow_mut())
    }

    /// Where the panel goes on an output of this size, in logical pixels.
    fn location(output_width: i32) -> Point<i32, Logical> {
        (
            output_width - PANEL_WIDTH - PANEL_MARGIN,
            BAR_HEIGHT + PANEL_MARGIN,
        )
            .into()
    }

    /// The part of the panel that's currently visible, relative to the output.
    fn visible_area(&self, output_width: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(
            Self::location(output_width),
            (PANEL_WIDTH, self.ui.get_panel_height().ceil() as i32).into(),
        )
    }

    /// Whether it has finished closing.
    fn is_closed(&self) -> bool {
        !self.ui.get_open() && !self.window.is_animating()
    }

    /// Handles a key, which has nowhere else to go while a panel is open.
    pub fn key(&self, key: &KeysymHandle<'_>, pressed: bool) {
        if !self.ui.get_open() {
            return;
        }

        if key.modified_sym().raw() == keysyms::KEY_Escape {
            if pressed {
                self.actions.borrow_mut().push(PanelAction::Close);
            }
        } else {
            self.window.text_key(key, pressed);
        }
    }

    fn update(&self, shell: &Shell, supports_night_light: bool) {
        let ui = &self.ui;
        let system = &shell.system;
        let status = &shell.status;

        let arrangement = if shell.editing {
            shell.tiles.arrange_for_editing()
        } else {
            shell.tiles.arrange()
        };
        let mut hidden = 0;
        let is_airplane = !system.wifi_enabled && !system.bluetooth_enabled;

        let tiles = TileKind::ALL.map(|kind| {
            let place = shell.tiles.tiles.iter().position(|tile| tile.kind == kind);
            let tile_place = place.map_or_else(
                || {
                    let (column, row) = Tiles::hidden_place(hidden);
                    hidden += 1;
                    Place {
                        page: 0,
                        column,
                        row: arrangement.rows + row,
                    }
                },
                |place| arrangement.places[place],
            );
            let on_off = |on: bool| if on { "On" } else { "Off" }.to_owned();

            let (active, subtitle) = match kind {
                TileKind::Wifi => (
                    system.wifi_enabled,
                    match &system.wifi_name {
                        _ if !system.wifi_enabled => "Off".to_owned(),
                        Some(name) => name.clone(),
                        None => "Not connected".to_owned(),
                    },
                ),
                TileKind::Bluetooth => (
                    system.bluetooth_enabled,
                    match &system.bluetooth_name {
                        _ if !system.bluetooth_enabled => "Off".to_owned(),
                        Some(name) => name.clone(),
                        None => "On".to_owned(),
                    },
                ),
                TileKind::Airplane => (is_airplane, on_off(is_airplane)),
                TileKind::Dark => (shell.dark, on_off(shell.dark)),
                TileKind::NightLight if !supports_night_light => {
                    (false, "Not on this display".to_owned())
                }
                TileKind::NightLight => (shell.night_light, on_off(shell.night_light)),
                TileKind::PowerSaver => match system.power_saver {
                    Some(on) => (on, on_off(on)),
                    None => (false, "Not available".to_owned()),
                },
                TileKind::StayAwake => (shell.stay_awake, on_off(shell.stay_awake)),
                TileKind::Microphone => (
                    !system.mic_muted,
                    if system.mic_muted { "Muted" } else { "On" }.to_owned(),
                ),
                TileKind::DoNotDisturb => (shell.do_not_disturb, on_off(shell.do_not_disturb)),
                TileKind::Audio => {
                    let muted = status.sound.is_some_and(|sound| sound.muted);
                    let output = system.sinks.iter().find(|sink| sink.active);
                    (
                        !muted,
                        match output {
                            _ if muted => "Muted".to_owned(),
                            Some(sink) => sink.description.clone(),
                            None => "No output".to_owned(),
                        },
                    )
                }
            };

            TileItem {
                kind: kind.index() as i32,
                label: kind.label().into(),
                subtitle: subtitle.into(),
                active,
                dual: kind.has_page(),
                large: shell.tiles.get(kind).is_some_and(|tile| tile.large),
                page: tile_place.page as i32,
                column: tile_place.column as i32,
                row: tile_place.row as i32,
                shown: place.is_some(),
                selected: shell.selected == Some(kind),
            }
        });

        update_model(&self.tiles, tiles.to_vec());

        ui.set_tile_pages(arrangement.pages as i32);
        ui.set_tile_rows(arrangement.rows as i32);
        ui.set_hidden_rows(shell.tiles.hidden_rows() as i32);
        ui.set_has_selection(shell.selected.is_some());
        ui.set_editing(shell.editing);
        ui.set_date(shell.date.as_str().into());
        ui.set_brightness(
            system
                .brightness
                .map_or(-1.0, |percent| percent as f32 / 100.0),
        );
        ui.set_volume(
            status
                .sound
                .map_or(0.0, |sound| sound.percent as f32 / 100.0),
        );
        ui.set_muted(status.sound.is_some_and(|sound| sound.muted));
        ui.set_wifi_enabled(system.wifi_enabled);
        ui.set_bluetooth_enabled(system.bluetooth_enabled);
        ui.set_mic_muted(system.mic_muted);
        ui.set_error(system.error.as_deref().unwrap_or_default().into());

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
    }
}

render_elements! {
    /// What's behind quick settings.
    BackdropElement<=smithay::backend::renderer::gles::GlesRenderer>;
    Wallpaper=MemoryRenderBufferRenderElement<smithay::backend::renderer::gles::GlesRenderer>,
    Surface=WlSurfaceTextureRenderElement,
}

/// Quick settings' render elements on `output`, from the top down.
pub fn panel_render_elements<R: PRenderer + AsGlesRenderer>(
    output: &Output,
    renderer: &mut R,
    space: &Space<WindowElement>,
) -> Vec<OutputRenderElement<R>> {
    let _span = tracy_client::span!("panel_render_elements");

    let Some(output_geo) = space.output_geometry(output) else {
        return Vec::new();
    };

    let needs_backdrop =
        output.with_state(|state| !blurred_panels(state, output_geo.size.w).is_empty());

    let mut elements = Vec::<OutputRenderElement<R>>::new();
    let output_scale = output.current_scale().fractional_scale();

    elements.extend(output.with_state(|state| {
        let toasts = state.toasts.as_ref().filter(|toasts| !toasts.is_empty())?;
        let location = OutputToasts::location(output_geo.size.w)
            .to_f64()
            .to_physical(output_scale)
            .to_i32_round();
        toasts.render_element(renderer, location).map(Into::into)
    }));

    if !needs_backdrop {
        return elements;
    }

    let scale = output.current_scale().fractional_scale();
    let small_scale = scale / DOWNSCALE;
    let output_size = output_geo
        .size
        .to_f64()
        .to_physical(scale)
        .to_i32_round::<i32>();

    let mut backdrop = Vec::<BackdropElement>::new();

    let wallpaper = output.with_state(|state| {
        let (buffer, pixel_size) = state.wallpaper.as_ref()?.buffer.as_ref()?;
        memory_element(
            renderer.as_gles_renderer(),
            Point::default(),
            buffer,
            *pixel_size,
            output_geo.size,
        )
    });
    backdrop.extend(wallpaper.map(BackdropElement::Wallpaper));

    let layers =
        |backdrop: &mut Vec<BackdropElement>, renderer: &mut R, kinds: &[wlr_layer::Layer]| {
            let map = layer_map_for_output(output);
            for kind in kinds {
                for layer in map.layers_on(*kind).rev() {
                    let Some(geo) = map.layer_geometry(layer) else {
                        continue;
                    };
                    let mut surfaces = texture_render_elements_from_surface_tree(
                        renderer.as_gles_renderer(),
                        layer.wl_surface(),
                        geo.loc.to_physical_precise_round(small_scale),
                        small_scale,
                        1.0,
                    );
                    surfaces.reverse();
                    backdrop.extend(surfaces.into_iter().map(BackdropElement::Surface));
                }
            }
        };

    layers(
        &mut backdrop,
        renderer,
        &[wlr_layer::Layer::Background, wlr_layer::Layer::Bottom],
    );

    for window in space.elements_for_output(output) {
        if !window.is_on_active_workspace() {
            continue;
        }
        let Some(location) = space.element_location(window) else {
            continue;
        };

        let elements = window.texture_render_elements(
            renderer,
            location - output_geo.loc,
            Scale::from(small_scale),
            1.0,
        );
        let mut surfaces = elements.surface_elements;
        surfaces.extend(elements.popup_elements);
        surfaces.reverse();
        backdrop.extend(surfaces.into_iter().map(BackdropElement::Surface));
    }

    layers(&mut backdrop, renderer, &[wlr_layer::Layer::Top]);

    output.with_state(|state| {
        for (window, blur, area, radius) in blurred_panels(state, output_geo.size.w) {
            let location = area.loc.to_f64().to_physical(scale).to_i32_round();
            elements.extend(window.render_element(renderer, location).map(Into::into));

            let region = area.to_f64().to_physical(scale).to_i32_round::<i32>();
            let blurred = blur.borrow_mut().render(
                renderer.as_gles_renderer(),
                backdrop.iter(),
                output_size,
                region,
                radius * scale as f32,
                Scale::from(small_scale),
            );
            match blurred {
                Ok(element) => elements.push(element.into()),
                Err(err) => warn!("Failed to blur behind a panel: {err:#}"),
            }
        }
    });

    elements
}

/// The panels on screen, as what they're drawn in, what to blur with,
/// what's visible of them and their corner radius.
fn blurred_panels(
    state: &OutputState,
    output_width: i32,
) -> Vec<(&ShellWindow, &RefCell<Blur>, Rectangle<i32, Logical>, f32)> {
    let quick_settings = state.panel.as_ref().map(|panel| {
        (
            &panel.window,
            &panel.blur,
            panel.visible_area(output_width),
            panel.ui.get_panel_radius(),
        )
    });
    let clock = state.clock.as_ref().map(|clock| {
        (
            &clock.window,
            &clock.blur,
            clock.visible_area(output_width),
            clock.ui.get_panel_radius(),
        )
    });
    let launcher = state.launcher.as_ref().map(|launcher| {
        (
            &launcher.window,
            &launcher.blur,
            launcher.visible_area(output_width),
            launcher.ui.get_panel_radius(),
        )
    });
    let settings = state.settings.as_ref().map(|settings| {
        (
            &settings.window,
            &settings.blur,
            settings.visible_area(output_width),
            settings.ui.get_panel_radius(),
        )
    });

    [quick_settings, clock, launcher, settings]
        .into_iter()
        .flatten()
        .filter(|(_, _, area, _)| area.size.h > 0)
        .collect()
}

/// The area of `output` that windows can use, relative to the output.
pub fn usable_zone(output: &Output) -> Rectangle<i32, Logical> {
    let mut zone = layer_map_for_output(output).non_exclusive_zone();

    if output.with_state(|state| state.bar.is_some()) {
        let overlap = (BAR_HEIGHT - zone.loc.y).clamp(0, zone.size.h);
        zone.loc.y += overlap;
        zone.size.h -= overlap;
    }

    zone
}

impl Pinnacle {
    /// Whether `location`, in the global space, is on `output`'s bar.
    pub fn bar_contains(&self, output: &Output, location: Point<f64, Logical>) -> bool {
        output.with_state(|state| state.bar.is_some())
            && self.space.output_geometry(output).is_some_and(|geo| {
                let bar = Rectangle::new(geo.loc, (geo.size.w, BAR_HEIGHT).into());
                bar.to_f64().contains(location)
            })
    }

    fn bar_content(&self, output: &Output) -> BarContent {
        let shown = output.with_state(|state| state.workspaces.clone());

        let workspaces = shown
            .iter()
            .map(|workspace| WorkspaceItem {
                name: workspace.name().into(),
                active: workspace.active(),
                occupied: self
                    .windows
                    .iter()
                    .any(|win| win.with_state(|state| state.workspaces.contains(workspace))),
            })
            .collect::<Vec<_>>();

        let active_index = workspaces
            .iter()
            .position(|item| item.active)
            .unwrap_or_default() as i32;

        let title = self
            .focus_stack_for_output(output)
            .rev()
            .find(|win| win.is_on_active_workspace() && !win.is_x11_override_redirect())
            .and_then(|win| win.title())
            .unwrap_or_default();

        BarContent {
            workspaces,
            active_index,
            title,
            clock_time: self.shell.clock_time.clone(),
            clock_date: self.shell.clock_date.clone(),
            has_notifications: !self.shell.notifications.is_empty(),
            do_not_disturb: self.shell.do_not_disturb,
            status: self.shell.status,
        }
    }

    /// The shell window on top at `location` in the global space, other than the bar,
    /// with where it is.
    pub fn shell_surface_under(
        &self,
        output: &Output,
        location: Point<f64, Logical>,
    ) -> Option<(ShellSurface, Point<f64, Logical>)> {
        let geo = self.space.output_geometry(output)?;

        output.with_state(|state| {
            let toasts = state
                .toasts
                .as_ref()
                .filter(|toasts| !toasts.is_empty())
                .map(|toasts| (ShellSurface::Toasts, toasts.visible_area(geo.size.w)));
            let clock = state
                .clock
                .as_ref()
                .map(|clock| (ShellSurface::Clock, clock.visible_area(geo.size.w)));
            let panel = state
                .panel
                .as_ref()
                .map(|panel| (ShellSurface::QuickSettings, panel.visible_area(geo.size.w)));
            let launcher = state
                .launcher
                .as_ref()
                .map(|launcher| (ShellSurface::Launcher, launcher.visible_area(geo.size.w)));
            let settings = state
                .settings
                .as_ref()
                .map(|settings| (ShellSurface::Settings, settings.visible_area(geo.size.w)));

            [toasts, panel, clock, launcher, settings]
                .into_iter()
                .flatten()
                .find_map(|(surface, mut area)| {
                    area.loc += geo.loc;
                    area.to_f64()
                        .contains(location)
                        .then(|| (surface, area.loc.to_f64()))
                })
        })
    }

    /// The output a panel is open on, which gets the keyboard.
    pub fn panel_output(&self) -> Option<WeakOutput> {
        self.shell
            .open_panel
            .as_ref()
            .map(|(output, _)| output.clone())
    }
}

impl State {
    /// Starts what the shell needs from outside the event loop.
    fn start_shell(&mut self) {
        let (sender, receiver) = channel::channel();

        let inserted = self
            .pinnacle
            .loop_handle
            .insert_source(receiver, |event, _, state| {
                if let Event::Msg(message) = event {
                    state.on_shell_message(message);
                }
            });
        if let Err(err) = inserted {
            error!("Failed to listen to the shell's threads: {err}");
            return;
        }

        let (commands, command_receiver) = mpsc::channel();
        self.pinnacle.shell.commands = Some(commands);
        self.pinnacle.shell.dark = true;

        system::spawn(
            command_receiver,
            {
                let sender = sender.clone();
                move |status| sender.send(Message::Status(status)).is_ok()
            },
            {
                let sender = sender.clone();
                move |system| sender.send(Message::System(Box::new(system))).is_ok()
            },
            {
                let sender = sender.clone();
                move |event| {
                    let _ = sender.send(Message::Notification(event));
                }
            },
        );

        let config_dir = self.pinnacle.config.config_dir.clone();
        self.pinnacle.shell.tiles = Tiles::read(&config_dir);
        self.pinnacle.shell.launch_counts = read_launch_counts();
        self.pinnacle.shell.settings = Settings::read(&config_dir);
        self.pinnacle.shell.sender = Some(sender);
    }

    fn on_shell_message(&mut self, message: Message) {
        match message {
            Message::Status(status) => self.pinnacle.shell.status = status,
            Message::System(system) => {
                self.pinnacle.shell.dark = system.dark;
                self.pinnacle.shell.system = *system;
            }
            Message::Notification(event) => self.on_notification(event),
            Message::Apps(apps) => {
                let shell = &mut self.pinnacle.shell;
                shell.apps = apps;
                shell.apps_generation = shell.apps_generation.wrapping_add(1);
            }
            Message::Files(files) => {
                self.pinnacle.shell.files = files;
                self.invalidate_launcher();
            }
            Message::Thumbnail(path, thumbnail) => {
                self.pinnacle.shell.wallpapers.push((path, thumbnail));
                self.pinnacle.shell.wallpapers.sort_by(|a, b| a.0.cmp(&b.0));
            }
            Message::Palette(palette) => {
                self.pinnacle.shell.palette = Some(palette);
                for output in self.pinnacle.outputs.clone() {
                    self.schedule_render(&output);
                }
            }
            Message::Wallpaper {
                output,
                size,
                image,
            } => {
                let Some(output) = self
                    .pinnacle
                    .outputs
                    .iter()
                    .find(|op| op.name() == output)
                    .cloned()
                else {
                    return;
                };

                let stored = output.with_state_mut(|state| {
                    // The output may have changed size since this was asked for.
                    let wallpaper = state.wallpaper.as_mut().filter(|w| w.requested.0 == size)?;
                    let buffer = MemoryRenderBuffer::from_slice(
                        image.as_raw(),
                        Fourcc::Abgr8888,
                        (size.w, size.h),
                        1,
                        Transform::Normal,
                        Some(vec![Rectangle::from_size((size.w, size.h).into())]),
                    );
                    wallpaper.buffer = Some((buffer, size));
                    Some(())
                });

                if stored.is_some() {
                    self.schedule_render(&output);
                }
            }
        }
    }

    /// Starts loading `output`'s wallpaper if it doesn't have one for its current size.
    fn request_wallpaper(&mut self, output: &Output, logical_size: Size<i32, Logical>) {
        let shell = &mut self.pinnacle.shell;
        let (Some(path), Some(sender)) = (shell.settings.wallpaper.clone(), shell.sender.clone())
        else {
            return;
        };

        let scale = output.current_scale().fractional_scale();
        let size = logical_size
            .to_f64()
            .to_physical(scale)
            .to_i32_round::<i32>();
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        let requested = (size, logical_size);

        let up_to_date = output.with_state_mut(|state| {
            let wallpaper = state.wallpaper.get_or_insert(OutputWallpaper {
                requested: Default::default(),
                buffer: None,
            });
            std::mem::replace(&mut wallpaper.requested, requested) == requested
        });
        if up_to_date {
            return;
        }

        let with_palette = !std::mem::replace(&mut shell.palette_requested, true);
        let name = output.name();

        let spawned = thread::Builder::new()
            .name("shell-wallpaper".into())
            .spawn(move || {
                let loaded = wallpaper::load(
                    &path,
                    size.w as u32,
                    size.h as u32,
                    with_palette,
                    |palette| {
                        let _ = sender.send(Message::Palette(palette));
                    },
                );

                match loaded {
                    Ok(image) => {
                        let _ = sender.send(Message::Wallpaper {
                            output: name,
                            size,
                            image,
                        });
                    }
                    Err(err) => warn!("Failed to load {}: {err}", path.display()),
                }
            });

        if let Err(err) = spawned {
            error!("Failed to start loading the wallpaper: {err}");
        }
    }

    fn send_command(&self, command: Command) {
        if let Some(commands) = &self.pinnacle.shell.commands {
            let _ = commands.send(command);
        }
    }

    fn on_notification(&mut self, event: notifications::Event) {
        let shell = &mut self.pinnacle.shell;

        match event {
            notifications::Event::Received(notification) => {
                let id = notification.id;
                let replaced = shell.notifications.iter().position(|n| n.id == id);
                if let Some(replaced) = replaced {
                    shell.notifications.remove(replaced);
                }
                shell.toasts.retain(|toast| toast.id != id);

                // Do Not Disturb lets through what can't wait,
                // and the clock panel already lists it right there.
                let clock_open = matches!(shell.open_panel, Some((_, ShellSurface::Clock)));
                if (!shell.do_not_disturb || notification.critical) && !clock_open {
                    shell.toasts.push(Toast {
                        id,
                        expires: notification.timeout.map(|timeout| Instant::now() + timeout),
                    });
                }

                shell.notifications.push(notification);
                if shell.notifications.len() > MAX_NOTIFICATIONS {
                    let oldest = shell.notifications.remove(0);
                    shell.toasts.retain(|toast| toast.id != oldest.id);
                }
            }
            notifications::Event::Closed(id) => {
                shell.notifications.retain(|n| n.id != id);
                shell.toasts.retain(|toast| toast.id != id);
                self.send_command(Command::NotificationClosed {
                    id,
                    reason: CloseReason::Closed,
                });
            }
        }

        for output in self.pinnacle.outputs.clone() {
            self.schedule_render(&output);
        }
    }

    fn dismiss_notification(&mut self, id: u32) {
        let shell = &mut self.pinnacle.shell;
        shell.notifications.retain(|n| n.id != id);
        shell.toasts.retain(|toast| toast.id != id);
        self.send_command(Command::NotificationClosed {
            id,
            reason: CloseReason::Dismissed,
        });
    }

    /// Lets the app know its notification was clicked, which is what brings it up.
    fn click_notification(&mut self, id: u32) {
        let has_action = self
            .pinnacle
            .shell
            .notifications
            .iter()
            .any(|n| n.id == id && n.has_default_action);

        if has_action {
            self.send_command(Command::NotificationClicked { id });
        }
        self.dismiss_notification(id);
    }

    fn toggle_do_not_disturb(&mut self) {
        let shell = &mut self.pinnacle.shell;
        shell.do_not_disturb = !shell.do_not_disturb;
        if shell.do_not_disturb {
            let notifications = &shell.notifications;
            shell
                .toasts
                .retain(|toast| notifications.iter().any(|n| n.id == toast.id && n.critical));
        }
    }

    /// Whether a panel other than this one is open on `output`.
    fn is_panel_open(&self, output: &Output, surface: ShellSurface) -> bool {
        self.pinnacle
            .shell
            .open_panel
            .as_ref()
            .is_some_and(|(open, open_surface)| {
                *open == output.downgrade() && *open_surface == surface
            })
    }

    fn toggle_quick_settings(&mut self, output: &Output) {
        let is_open = self.is_panel_open(output, ShellSurface::QuickSettings);

        self.close_panels();

        if is_open {
            return;
        }

        let Some(geo) = self.pinnacle.space.output_geometry(output) else {
            return;
        };
        let scale = output.current_scale().fractional_scale();
        let height = (geo.size.h - BAR_HEIGHT - PANEL_MARGIN * 2).clamp(1, PANEL_MAX_HEIGHT);

        let reopened = output.with_state(|state| {
            state.panel.as_ref().is_some_and(|panel| {
                let fits = panel.window.matches((PANEL_WIDTH, height).into(), scale);
                if fits {
                    panel.ui.set_open(true);
                }
                fits
            })
        });

        if !reopened {
            match OutputPanel::new(height, scale) {
                Ok(panel) => output.with_state_mut(|state| state.panel = Some(panel)),
                Err(err) => {
                    error!("Failed to open quick settings: {err}");
                    return;
                }
            }
        }

        self.pinnacle.shell.open_panel = Some((output.downgrade(), ShellSurface::QuickSettings));
        self.send_command(Command::Watch(Watching::Tiles));
        self.schedule_render(output);
    }

    fn toggle_clock(&mut self, output: &Output) {
        let is_open = self.is_panel_open(output, ShellSurface::Clock);

        self.close_panels();

        if is_open {
            return;
        }

        let Some(geo) = self.pinnacle.space.output_geometry(output) else {
            return;
        };
        let scale = output.current_scale().fractional_scale();
        let height = (geo.size.h - BAR_HEIGHT - PANEL_MARGIN * 2).clamp(1, CLOCK_MAX_HEIGHT);

        let reopened = output.with_state(|state| {
            state.clock.as_ref().is_some_and(|clock| {
                let fits = clock.window.matches((CLOCK_WIDTH, height).into(), scale);
                if fits {
                    clock.ui.set_open(true);
                }
                fits
            })
        });

        if !reopened {
            match OutputClock::new(height, scale) {
                Ok(clock) => output.with_state_mut(|state| state.clock = Some(clock)),
                Err(err) => {
                    error!("Failed to open the clock panel: {err}");
                    return;
                }
            }
        }

        self.pinnacle.shell.calendar_offset = 0;
        // Everything popping up is in the panel too, and would be in front of it.
        self.pinnacle.shell.toasts.clear();
        self.pinnacle.shell.open_panel = Some((output.downgrade(), ShellSurface::Clock));
        self.send_command(Command::Watch(Watching::Clock));
        self.schedule_render(output);
    }

    /// Closes whichever panel is open.
    pub fn close_panels(&mut self) {
        let Some(output) = self
            .pinnacle
            .shell
            .open_panel
            .take()
            .and_then(|(output, _)| output.upgrade())
        else {
            return;
        };

        output.with_state(|state| {
            if let Some(panel) = state.panel.as_ref() {
                panel.ui.set_open(false);
                panel.ui.set_page(Page::Tiles);
            }
            if let Some(clock) = state.clock.as_ref() {
                clock.ui.set_open(false);
            }
            if let Some(launcher) = state.launcher.as_ref() {
                launcher.ui.set_open(false);
            }
            if let Some(settings) = state.settings.as_ref() {
                settings.ui.set_open(false);
            }
        });

        let shell = &mut self.pinnacle.shell;
        shell.editing = false;
        shell.selected = None;

        self.send_command(Command::Watch(Watching::Nothing));
        self.schedule_render(&output);
    }

    /// Handles a tile being clicked while the tiles are being edited.
    fn on_tile_edited(&mut self, kind: TileKind) {
        let shell = &mut self.pinnacle.shell;

        if shell.tiles.get(kind).is_none() {
            shell.tiles.add(kind);
            self.save_tiles();
            return;
        }

        match shell.selected {
            // Clicking the selected tile lets go of it.
            Some(selected) if selected == kind => shell.selected = None,
            // Clicking another one puts the selected tile there.
            Some(selected) => {
                shell.tiles.move_to(selected, kind);
                shell.selected = None;
                self.save_tiles();
            }
            None => shell.selected = Some(kind),
        }
    }

    fn on_tile_clicked(&mut self, kind: TileKind) {
        let shell = &mut self.pinnacle.shell;

        let system = &shell.system;
        let command = match kind {
            TileKind::Wifi => Command::SetWifi(!system.wifi_enabled),
            TileKind::Bluetooth => Command::SetBluetooth(!system.bluetooth_enabled),
            TileKind::Airplane => {
                Command::SetAirplane(system.wifi_enabled || system.bluetooth_enabled)
            }
            TileKind::Dark => {
                shell.dark = !shell.dark;
                shell.system.dark = shell.dark;
                Command::SetDark(shell.dark)
            }
            TileKind::PowerSaver => {
                let Some(on) = system.power_saver else {
                    return;
                };
                Command::SetPowerSaver(!on)
            }
            TileKind::Microphone => Command::SetMicMuted(!system.mic_muted),
            TileKind::Audio => {
                Command::SetMuted(!shell.status.sound.is_some_and(|sound| sound.muted))
            }
            TileKind::StayAwake => {
                shell.stay_awake = !shell.stay_awake;
                return;
            }
            TileKind::DoNotDisturb => {
                self.toggle_do_not_disturb();
                return;
            }
            TileKind::NightLight => {
                shell.night_light = !shell.night_light;
                shell.night_light_outputs.clear();
                return;
            }
        };

        self.send_command(command);
    }

    /// Starts a program that's not ours to look after.
    fn spawn(&self, mut command: std::process::Command) {
        let spawned = command
            .env("WAYLAND_DISPLAY", &self.pinnacle.socket_name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // It shouldn't go down with us, or get our signals.
            .process_group(0)
            .spawn();

        match spawned {
            // Something has to collect it when it exits.
            Ok(mut child) => {
                let waited = thread::Builder::new()
                    .name("shell-child".into())
                    .spawn(move || child.wait());
                if let Err(err) = waited {
                    warn!("Failed to start a thread: {err}");
                }
            }
            Err(err) => error!("Failed to start {:?}: {err}", command.get_program()),
        }
    }

    fn open_quick_settings_page_in_settings(&mut self, page: Page) {
        let page = match page {
            Page::Bluetooth => SettingsPage::Devices,
            Page::Sound => SettingsPage::Sound,
            Page::Power => SettingsPage::Battery,
            Page::Tiles | Page::Wifi => SettingsPage::Network,
        };
        self.open_settings(page);
    }

    /// Opens the settings app on `page`.
    fn open_settings(&mut self, page: SettingsPage) {
        let Some(output) = self.pinnacle.focused_output().cloned() else {
            return;
        };

        self.close_panels();

        let Some(geo) = self.pinnacle.space.output_geometry(&output) else {
            return;
        };
        let scale = output.current_scale().fractional_scale();
        let size = settings_app::size(geo.size);

        let reopened = output.with_state(|state| {
            state.settings.as_ref().is_some_and(|settings| {
                let fits = settings.window.matches(size, scale);
                if fits {
                    settings.ui.set_open(true);
                }
                fits
            })
        });

        if !reopened {
            match OutputSettings::new(size, scale) {
                Ok(settings) => output.with_state_mut(|state| state.settings = Some(settings)),
                Err(err) => {
                    error!("Failed to open settings: {err}");
                    return;
                }
            }
        }

        self.pinnacle.shell.open_panel = Some((output.downgrade(), ShellSurface::Settings));
        self.open_settings_page(&output, page);
        self.schedule_render(&output);
    }

    fn open_settings_page(&mut self, output: &Output, page: SettingsPage) {
        output.with_state(|state| {
            if let Some(settings) = state.settings.as_ref() {
                settings.ui.set_page(page);
            }
        });
        self.send_command(Command::Watch(match page {
            SettingsPage::Network => Watching::Wifi,
            SettingsPage::Devices => Watching::Bluetooth,
            SettingsPage::Sound => Watching::Sound,
            _ => Watching::Tiles,
        }));

        if page == SettingsPage::Style {
            self.request_wallpapers();
        }
    }

    /// Starts making small versions of the wallpapers to choose from.
    fn request_wallpapers(&mut self) {
        let shell = &mut self.pinnacle.shell;
        let Some(sender) = shell.sender.clone() else {
            return;
        };
        if std::mem::replace(&mut shell.wallpapers_requested, true) {
            return;
        }

        let mut dirs = shell
            .settings
            .wallpaper
            .iter()
            .filter_map(|path| Some(path.parent()?.to_owned()))
            .collect::<Vec<_>>();
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join("Pictures/Wallpapers"));
        }
        dirs.sort();
        dirs.dedup();

        let spawned = thread::Builder::new()
            .name("shell-thumbnails".into())
            .spawn(move || {
                let paths = dirs
                    .iter()
                    .filter_map(|dir| fs::read_dir(dir).ok())
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| wallpaper::is_image(path));

                for path in paths {
                    if let Some(thumbnail) = wallpaper::thumbnail(&path) {
                        let _ = sender.send(Message::Thumbnail(path, thumbnail));
                    }
                }
            });

        if let Err(err) = spawned {
            error!("Failed to start finding wallpapers: {err}");
        }
    }

    /// Uses another wallpaper, and the colors that come with it.
    fn set_wallpaper(&mut self, path: PathBuf) {
        let path = path.to_string_lossy().into_owned();
        Settings::write_wallpaper(&self.pinnacle.config.config_dir, &path);

        let shell = &mut self.pinnacle.shell;
        shell.settings.wallpaper = Some(PathBuf::from(path));
        shell.palette_requested = false;

        for output in &self.pinnacle.outputs {
            output.with_state_mut(|state| {
                // The old one stays until the new one has loaded.
                if let Some(wallpaper) = state.wallpaper.as_mut() {
                    wallpaper.requested = Default::default();
                }
            });
        }
    }

    fn set_output_scale(&mut self, name: &str, scale: f32) {
        let output = self
            .pinnacle
            .outputs
            .iter()
            .find(|output| output.name() == name)
            .cloned();
        let Some(output) = output else {
            return;
        };

        self.pinnacle.change_output_state(
            &mut self.backend,
            &output,
            None,
            None,
            Some(smithay::output::Scale::Fractional(scale.into())),
            None,
        );
        self.pinnacle.request_layout(&output);
        self.schedule_render(&output);
        self.pinnacle
            .output_management_manager_state
            .update::<State>();

        // It was laid out for the old scale, so it has to start over at the new one.
        let page = output.with_state_mut(|state| {
            let page = state
                .settings
                .as_ref()
                .map(|settings| settings.ui.get_page());
            state.settings = None;
            page
        });
        if let Some(page) = page {
            self.open_settings(page);
        }
    }

    fn on_settings_action(&mut self, output: &Output, action: SettingsAction) {
        match action {
            SettingsAction::Close => self.close_panels(),
            SettingsAction::Query => (),
            SettingsAction::OpenPage(page) => self.open_settings_page(output, page),
            SettingsAction::ToggleWifi => {
                let enabled = self.pinnacle.shell.system.wifi_enabled;
                self.send_command(Command::SetWifi(!enabled));
            }
            SettingsAction::ToggleBluetooth => {
                let enabled = self.pinnacle.shell.system.bluetooth_enabled;
                self.send_command(Command::SetBluetooth(!enabled));
            }
            SettingsAction::ToggleDark => self.on_tile_clicked(TileKind::Dark),
            SettingsAction::ToggleNightLight => self.on_tile_clicked(TileKind::NightLight),
            SettingsAction::ToggleMute => self.on_tile_clicked(TileKind::Audio),
            SettingsAction::ToggleMicrophone => self.on_tile_clicked(TileKind::Microphone),
            SettingsAction::ToggleDoNotDisturb => self.toggle_do_not_disturb(),
            SettingsAction::TogglePowerSaver => self.on_tile_clicked(TileKind::PowerSaver),
            SettingsAction::ToggleStayAwake => self.on_tile_clicked(TileKind::StayAwake),
            SettingsAction::ClearNotifications => self.clear_notifications(),
            SettingsAction::SetScale(name, scale) => self.set_output_scale(&name, scale),
            SettingsAction::SetWallpaper(path) => self.set_wallpaper(path),
            SettingsAction::Run(command) => self.send_command(command),
        }
    }

    fn clear_notifications(&mut self) {
        let ids = self
            .pinnacle
            .shell
            .notifications
            .iter()
            .map(|notification| notification.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.dismiss_notification(id);
        }
    }

    fn invalidate_launcher(&self) {
        for output in &self.pinnacle.outputs {
            output.with_state(|state| {
                if let Some(launcher) = state.launcher.as_ref() {
                    launcher.invalidate();
                }
            });
        }
    }

    /// Looks for apps and files, which may have changed since last time.
    fn refresh_search(&mut self) {
        let shell = &mut self.pinnacle.shell;
        let Some(sender) = shell.sender.clone() else {
            return;
        };
        if shell
            .apps_scanned
            .is_some_and(|scanned| scanned.elapsed() < SEARCH_REFRESH_INTERVAL)
        {
            return;
        }
        shell.apps_scanned = Some(Instant::now());

        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .ok()
            .and_then(|desktops| Some(desktops.split(':').next()?.to_owned()))
            .unwrap_or_else(|| "Pinnacle".to_owned());
        let files_sender = sender.clone();

        let spawned = thread::Builder::new()
            .name("shell-search".into())
            .spawn(move || {
                let theme = system::gsettings(&["get", system::INTERFACE_SCHEMA, "icon-theme"])
                    .map(|theme| theme.trim_matches('\'').to_owned())
                    .unwrap_or_else(|| "hicolor".to_owned());

                apps::scan(desktop, theme, move |apps| {
                    let _ = sender.send(Message::Apps(apps));
                });

                if let Some(home) = std::env::var_os("HOME") {
                    let files = search::index_files(Path::new(&home));
                    let _ = files_sender.send(Message::Files(files));
                }
            });

        if let Err(err) = spawned {
            error!("Failed to start looking for apps: {err}");
        }
    }

    fn toggle_launcher(&mut self, output: &Output) {
        let is_open = self.is_panel_open(output, ShellSurface::Launcher);

        self.close_panels();

        if is_open {
            return;
        }

        self.refresh_search();

        let Some(geo) = self.pinnacle.space.output_geometry(output) else {
            return;
        };
        let scale = output.current_scale().fractional_scale();
        let size = launcher::size(geo.size);

        let reopened = output.with_state(|state| {
            state.launcher.as_ref().is_some_and(|launcher| {
                let fits = launcher.window.matches(size, scale);
                if fits {
                    launcher.reopen(geo.size.h);
                }
                fits
            })
        });

        if !reopened {
            match OutputLauncher::new(size, geo.size.h, scale) {
                Ok(launcher) => output.with_state_mut(|state| state.launcher = Some(launcher)),
                Err(err) => {
                    error!("Failed to open the launcher: {err}");
                    return;
                }
            }
        }

        self.pinnacle.shell.open_panel = Some((output.downgrade(), ShellSurface::Launcher));
        self.send_command(Command::Watch(Watching::Tiles));
        self.schedule_render(output);
    }

    fn launch_app(&mut self, id: &str) {
        let shell = &mut self.pinnacle.shell;
        let Some(app) = shell.apps.iter().find(|app| app.id == id) else {
            return;
        };

        let mut command = if app.terminal {
            let mut command = std::process::Command::new("x-terminal-emulator");
            command.args(["-e", "sh", "-c", &app.command]);
            command
        } else {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", &format!("exec {}", app.command)]);
            command
        };
        command.current_dir(std::env::var_os("HOME").unwrap_or_else(|| "/".into()));

        *shell.launch_counts.entry(id.to_owned()).or_default() += 1;
        write_launch_counts(&shell.launch_counts);

        self.spawn(command);
    }

    fn open_target(&mut self, target: Target) {
        match target {
            Target::App(id) => {
                self.close_panels();
                self.launch_app(&id);
            }
            Target::Settings(topic) => self.open_settings(PAGES[topic]),
            Target::File(path) => {
                self.close_panels();
                let mut command = std::process::Command::new("xdg-open");
                command.arg(path);
                self.spawn(command);
            }
            Target::Web(url) => {
                self.close_panels();
                let mut command = std::process::Command::new("xdg-open");
                command.arg(url);
                self.spawn(command);
            }
            Target::Nothing => (),
        }
    }

    fn on_launcher_action(&mut self, output: &Output, action: LauncherAction) {
        let with_launcher = |func: &dyn Fn(&OutputLauncher)| {
            output.with_state(|state| {
                if let Some(launcher) = state.launcher.as_ref() {
                    func(launcher);
                }
            });
        };

        match action {
            LauncherAction::Close => self.close_panels(),
            LauncherAction::Query => with_launcher(&OutputLauncher::on_query_changed),
            LauncherAction::Move { columns, rows } => {
                with_launcher(&|launcher| launcher.move_selection(columns, rows));
            }
            LauncherAction::Choose => {
                let target = output.with_state(|state| {
                    state
                        .launcher
                        .as_ref()
                        .and_then(OutputLauncher::selected_target)
                });
                if let Some(target) = target {
                    self.open_target(target);
                }
            }
            LauncherAction::ChooseApp(id) => self.open_target(Target::App(id)),
            LauncherAction::ChooseResult(index) => {
                let target = output.with_state(|state| {
                    state
                        .launcher
                        .as_ref()
                        .and_then(|launcher| launcher.result_target(index))
                });
                if let Some(target) = target {
                    self.open_target(target);
                }
            }
        }
    }

    /// Whether a key press is the shortcut for the launcher, which is then taken care of.
    ///
    /// This is the same mod key as in the default config, which is Alt in a window,
    /// because the compositor around it wants Super for itself.
    pub fn on_launcher_shortcut(&mut self, raw_sym: u32, modifiers: &ModifiersState) -> bool {
        let is_mod = match self.backend {
            Backend::Winit(_) => modifiers.alt,
            _ => modifiers.logo,
        };
        let is_shortcut =
            raw_sym == keysyms::KEY_space && is_mod && !modifiers.ctrl && !modifiers.shift;

        if is_shortcut {
            self.pinnacle.shell.launcher_requested = true;
        }
        is_shortcut
    }

    /// Keeps track of Super being tapped, which is what opens the launcher.
    pub fn on_key_for_launcher(&mut self, raw_sym: u32, pressed: bool) {
        let shell = &mut self.pinnacle.shell;
        let is_super = matches!(raw_sym, keysyms::KEY_Super_L | keysyms::KEY_Super_R);

        if pressed {
            shell.super_tapping = is_super;
        } else if is_super && std::mem::take(&mut shell.super_tapping) {
            shell.launcher_requested = true;
        }
    }

    fn save_tiles(&self) {
        self.pinnacle
            .shell
            .tiles
            .write(&self.pinnacle.config.config_dir);
    }

    fn open_page(&mut self, output: &Output, page: Page) {
        output.with_state(|state| {
            if let Some(panel) = state.panel.as_ref() {
                panel.ui.set_page(page);
            }
        });
        self.send_command(Command::Watch(match page {
            Page::Tiles | Page::Power => Watching::Tiles,
            Page::Wifi => Watching::Wifi,
            Page::Bluetooth => Watching::Bluetooth,
            Page::Sound => Watching::Sound,
        }));
    }

    fn on_panel_action(&mut self, output: &Output, action: PanelAction) {
        match action {
            PanelAction::Close => self.close_panels(),
            PanelAction::ToggleEditing => {
                let shell = &mut self.pinnacle.shell;
                shell.editing = !shell.editing;
                shell.selected = None;
            }
            PanelAction::EditTile(kind) => self.on_tile_edited(kind),
            PanelAction::ToggleTile(kind) => self.on_tile_clicked(kind),
            PanelAction::OpenTile(kind) => match kind {
                TileKind::Wifi => self.open_page(output, Page::Wifi),
                TileKind::Bluetooth => self.open_page(output, Page::Bluetooth),
                TileKind::Microphone | TileKind::Audio => self.open_page(output, Page::Sound),
                _ => (),
            },
            PanelAction::ResizeTile(kind) => {
                self.pinnacle.shell.tiles.toggle_size(kind);
                self.save_tiles();
            }
            PanelAction::RemoveSelectedTile => {
                if let Some(kind) = self.pinnacle.shell.selected.take() {
                    self.pinnacle.shell.tiles.remove(kind);
                    self.save_tiles();
                }
            }
            PanelAction::OpenPage(page) => self.open_page(output, page),
            PanelAction::OpenSettings(page) => self.open_quick_settings_page_in_settings(page),
            PanelAction::ToggleWifi => {
                let enabled = self.pinnacle.shell.system.wifi_enabled;
                self.send_command(Command::SetWifi(!enabled));
            }
            PanelAction::ToggleBluetooth => {
                let enabled = self.pinnacle.shell.system.bluetooth_enabled;
                self.send_command(Command::SetBluetooth(!enabled));
            }
            PanelAction::ToggleMicrophone => {
                let muted = self.pinnacle.shell.system.mic_muted;
                self.send_command(Command::SetMicMuted(!muted));
            }
            PanelAction::LogOut => self.pinnacle.shutdown(),
            PanelAction::Run(command) => self.send_command(command),
        }
    }

    fn on_clock_action(&mut self, action: ClockAction) {
        match action {
            ClockAction::Close => self.close_panels(),
            ClockAction::ChangeMonth(change) => self.pinnacle.shell.calendar_offset += change,
            ClockAction::GoToToday => self.pinnacle.shell.calendar_offset = 0,
            ClockAction::ClickNotification(id) => self.click_notification(id),
            ClockAction::DismissNotification(id) => self.dismiss_notification(id),
            ClockAction::ClearNotifications => self.clear_notifications(),
            ClockAction::ToggleDoNotDisturb => self.toggle_do_not_disturb(),
            ClockAction::MediaPlayPause => self.send_command(Command::MediaPlayPause),
            ClockAction::MediaPrevious => self.send_command(Command::MediaPrevious),
            ClockAction::MediaNext => self.send_command(Command::MediaNext),
            ClockAction::MediaSeek(position) => self.send_command(Command::MediaSeek(position)),
        }
    }

    /// Does what the user asked for through the shell's UI.
    pub fn process_shell_input(&mut self) {
        if std::mem::take(&mut self.pinnacle.shell.launcher_requested)
            && self.pinnacle.lock_state.is_unlocked()
            && let Some(output) = self.pinnacle.focused_output().cloned()
        {
            self.toggle_launcher(&output);
        }

        for output in self.pinnacle.outputs.clone() {
            let actions = output.with_state(|state| {
                state
                    .bar
                    .as_ref()
                    .map(OutputBar::take_actions)
                    .unwrap_or_default()
            });

            for action in actions {
                match action {
                    BarAction::Workspace(index) => {
                        let workspace =
                            output.with_state(|state| state.workspaces.get_index(index).cloned());
                        if let Some(workspace) = workspace {
                            switch_to(self, &workspace);
                        }
                    }
                    BarAction::QuickSettings => self.toggle_quick_settings(&output),
                    BarAction::Clock => self.toggle_clock(&output),
                    BarAction::Launcher => self.toggle_launcher(&output),
                }
            }

            let actions = output.with_state(|state| {
                state
                    .panel
                    .as_ref()
                    .map(OutputPanel::take_actions)
                    .unwrap_or_default()
            });

            for action in actions {
                self.on_panel_action(&output, action);
            }

            let actions = output.with_state(|state| {
                state
                    .launcher
                    .as_ref()
                    .map(OutputLauncher::take_actions)
                    .unwrap_or_default()
            });

            for action in actions {
                self.on_launcher_action(&output, action);
            }

            let actions = output.with_state(|state| {
                state
                    .settings
                    .as_ref()
                    .map(OutputSettings::take_actions)
                    .unwrap_or_default()
            });

            for action in actions {
                self.on_settings_action(&output, action);
            }

            let actions = output.with_state(|state| {
                state
                    .clock
                    .as_ref()
                    .map(OutputClock::take_actions)
                    .unwrap_or_default()
            });

            for action in actions {
                self.on_clock_action(action);
            }

            let actions = output.with_state(|state| {
                state
                    .toasts
                    .as_ref()
                    .map(OutputToasts::take_actions)
                    .unwrap_or_default()
            });

            for action in actions {
                match action {
                    ToastAction::Click(id) => self.click_notification(id),
                    ToastAction::Dismiss(id) => self.dismiss_notification(id),
                }
            }
        }
    }

    /// Tints the outputs if Night Light is on and it hasn't been done yet.
    fn refresh_night_light(&mut self) {
        let Backend::Udev(udev) = &mut self.backend else {
            return;
        };
        let shell = &mut self.pinnacle.shell;

        for output in &self.pinnacle.outputs {
            let weak = output.downgrade();
            if shell.night_light_outputs.contains(&weak) {
                continue;
            }
            shell.night_light_outputs.push(weak);

            let result = if shell.night_light {
                udev.gamma_size(output).and_then(|size| {
                    let [red, green, blue] = NIGHT_LIGHT.map(|channel| ramp(size, channel));
                    udev.set_gamma(output, Some([&red, &green, &blue]))
                })
            } else {
                udev.set_gamma(output, None)
            };

            if let Err(err) = result {
                warn!("Failed to set Night Light on {}: {err}", output.name());
            }
        }
    }

    /// Brings the shell up to date and redraws what changed.
    pub fn refresh_shell(&mut self) {
        let _span = tracy_client::span!("State::refresh_shell");

        if self.pinnacle.shell.disabled {
            return;
        }

        // Tests expect windows to get the whole output.
        #[cfg(feature = "testing")]
        if matches!(self.backend, crate::backend::Backend::Dummy(_)) {
            return;
        }

        if self.pinnacle.shell.sender.is_none() {
            self.start_shell();
        }

        slint::platform::update_timers_and_animations();

        self.pinnacle.shell.update_clock();
        self.refresh_night_light();

        let now = Instant::now();
        self.pinnacle
            .shell
            .toasts
            .retain(|toast| toast.expires.is_none_or(|expires| expires > now));
        let focused_output = self.pinnacle.focused_output().cloned();

        let supports_night_light = matches!(self.backend, Backend::Udev(_));
        let output_details = self
            .pinnacle
            .outputs
            .iter()
            .map(|output| OutputDetails {
                name: output.name(),
                resolution: output
                    .current_mode()
                    .map(|mode| format!("{} × {}", mode.size.w, mode.size.h))
                    .unwrap_or_default(),
                scale: output.current_scale().fractional_scale(),
            })
            .collect::<Vec<_>>();
        let palette = self
            .pinnacle
            .shell
            .palette
            .map(|palettes| *palettes.get(self.pinnacle.shell.dark));

        for output in self.pinnacle.outputs.clone() {
            let Some(geo) = self.pinnacle.space.output_geometry(&output) else {
                continue;
            };
            let scale = output.current_scale().fractional_scale();

            let (had_bar, up_to_date) = output.with_state(|state| {
                let bar = state.bar.as_ref();
                (
                    bar.is_some(),
                    bar.is_some_and(|bar| {
                        bar.window.matches((geo.size.w, BAR_HEIGHT).into(), scale)
                    }),
                )
            });

            if !up_to_date {
                match OutputBar::new(geo.size.w, scale) {
                    Ok(bar) => output.with_state_mut(|state| state.bar = Some(bar)),
                    Err(err) => {
                        error!("Failed to create the bar, running without a shell: {err}");
                        self.pinnacle.shell.disabled = true;
                        return;
                    }
                }
                if !had_bar {
                    self.pinnacle.request_layout(&output);
                }
            }

            self.request_wallpaper(&output, geo.size);

            let content = self.pinnacle.bar_content(&output);
            let shell = &self.pinnacle.shell;
            let is_focused = focused_output.as_ref() == Some(&output);
            let (drawn, panel_visible) = output.with_state_mut(|state| {
                if let Some(palette) = palette {
                    state.border_colors = palette.border_colors();
                }

                let bar = state.bar.as_mut().expect("the bar was just created");
                bar.update(content);
                if let Some(palette) = &palette {
                    apply_palette(Theme::get(&bar.ui), palette);
                }
                let mut drawn = bar.window.draw();

                if state.panel.as_ref().is_some_and(OutputPanel::is_closed) {
                    state.panel = None;
                    drawn = true;
                }

                if state.clock.as_ref().is_some_and(OutputClock::is_closed) {
                    state.clock = None;
                    drawn = true;
                }

                // Pop-ups go on the focused output.
                let wants_toasts = !shell.toasts.is_empty() && is_focused;
                if wants_toasts
                    && state
                        .toasts
                        .as_ref()
                        .is_none_or(|toasts| !toasts.window.matches_scale(scale))
                {
                    match OutputToasts::new(scale) {
                        Ok(toasts) => state.toasts = Some(toasts),
                        Err(err) => error!("Failed to show a notification: {err}"),
                    }
                }
                if !wants_toasts && state.toasts.take().is_some() {
                    drawn = true;
                }
                if let Some(toasts) = state.toasts.as_mut() {
                    toasts.update(shell);
                    if let Some(palette) = &palette {
                        apply_palette(Theme::get(&toasts.ui), palette);
                    }
                    drawn |= toasts.window.draw();
                }

                if state
                    .launcher
                    .as_ref()
                    .is_some_and(OutputLauncher::is_closed)
                {
                    state.launcher = None;
                    drawn = true;
                }
                if state
                    .settings
                    .as_ref()
                    .is_some_and(OutputSettings::is_closed)
                {
                    state.settings = None;
                    drawn = true;
                }

                let panel_visible = state.panel.is_some()
                    || state.clock.is_some()
                    || state.launcher.is_some()
                    || state.settings.is_some();

                if let Some(launcher) = state.launcher.as_mut() {
                    launcher.update(shell);
                    if let Some(palette) = &palette {
                        apply_palette(Theme::get(&launcher.ui), palette);
                    }
                    drawn |= launcher.window.draw();
                }
                if let Some(settings) = state.settings.as_mut() {
                    settings.update(shell, &output_details, supports_night_light);
                    if let Some(palette) = &palette {
                        apply_palette(Theme::get(&settings.ui), palette);
                    }
                    drawn |= settings.window.draw();
                }

                if let Some(clock) = state.clock.as_mut() {
                    clock.update(shell);
                    if let Some(palette) = &palette {
                        apply_palette(Theme::get(&clock.ui), palette);
                    }
                    drawn |= clock.window.draw();
                }

                if let Some(panel) = state.panel.as_mut() {
                    panel.update(shell, supports_night_light);
                    if let Some(palette) = &palette {
                        apply_palette(Theme::get(&panel.ui), palette);
                    }
                    drawn |= panel.window.draw();
                }

                (drawn, panel_visible)
            });

            // The blur behind a panel has to keep up with what's going on behind it.
            if drawn || panel_visible {
                self.schedule_render(&output);
            }
        }
    }
}

/// A gamma ramp of `size` entries that scales a channel by `factor`.
fn ramp(size: u32, factor: f64) -> Vec<u16> {
    (0..size)
        .map(|index| {
            let position = index as f64 / size.saturating_sub(1).max(1) as f64;
            (position * factor * u16::MAX as f64).round() as u16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_light_ramp() {
        assert_eq!(ramp(3, 1.0), [0, 32768, 65535]);
        assert_eq!(ramp(2, 0.5), [0, 32768]);
    }
}
