// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Window management.
//!
//! This module provides ways to get [`WindowHandle`]s and move and resize
//! windows using the mouse.
//!
//! [`WindowHandle`]s allow you to do things like resize and move windows, toggle them between
//! floating and tiled, close them, and more.

use std::borrow::Borrow;

use futures::FutureExt;
use pinnacle_api_defs::pinnacle::{
    util::v1::SetOrToggle,
    window::{
        self,
        v1::{
            GetAppIdRequest, GetFocusedRequest, GetForeignToplevelListIdentifierRequest,
            GetLayoutModeRequest, GetLocRequest, GetSizeRequest, GetWorkspaceIdsRequest, GetTitleRequest,
            GetWindowsInDirRequest, LowerRequest, MoveGrabRequest, MoveToOutputRequest,
            MoveToWorkspaceRequest, RaiseRequest, ResizeGrabRequest, ResizeTileRequest,
            SetDecorationModeRequest, SetFloatingRequest, SetFocusedRequest, SetFullscreenRequest,
            SetGeometryRequest, SetMaximizedRequest, SetWorkspaceRequest, SetWorkspacesRequest,
            SetVrrDemandRequest, SwapRequest,
        },
    },
};
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::StreamExt;

use crate::{
    BlockOnTokio,
    client::Client,
    input::MouseButton,
    output::OutputHandle,
    signal::{SignalHandle, WindowSignal},
    workspace::WorkspaceHandle,
    util::{Batch, Direction, Point, Size},
};

/// Gets handles to all windows.
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::window;
/// for win in window::get_all() {
///     println!("{}", win.title());
/// }
/// ```
pub fn get_all() -> impl Iterator<Item = WindowHandle> {
    get_all_async().block_on_tokio()
}

/// Async impl for [`get_all`].
pub async fn get_all_async() -> impl Iterator<Item = WindowHandle> {
    let window_ids = Client::window()
        .get(pinnacle_api_defs::pinnacle::window::v1::GetRequest {})
        .await
        .unwrap()
        .into_inner()
        .window_ids;

    window_ids.into_iter().map(|id| WindowHandle { id })
}

/// Gets a handle to the window with the current keyboard focus.
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::window;
/// if let Some(focused) = window::get_focused() {
///     println!("{}", focused.title());
/// }
/// ```
pub fn get_focused() -> Option<WindowHandle> {
    get_focused_async().block_on_tokio()
}

/// Async impl for [`get_focused`].
pub async fn get_focused_async() -> Option<WindowHandle> {
    let windows = get_all_async().await;

    windows.batch_find(|win| win.focused_async().boxed(), |focused| *focused)
}

/// Begins an interactive window move.
///
/// This will start moving the window under the pointer until `button` is released.
///
/// `button` should be the mouse button that is held at the time
/// this function is called. Otherwise, the move will not start.
/// This is intended for use in tandem with a mousebind.
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::window;
/// # use pinnacle_api::input;
/// # use pinnacle_api::input::Mod;
/// # use pinnacle_api::input::MouseButton;
/// input::mousebind(Mod::SUPER, MouseButton::Left)
///     .on_press(|| window::begin_move(MouseButton::Left));
/// ```
pub fn begin_move(button: MouseButton) {
    Client::window()
        .move_grab(MoveGrabRequest {
            button: button.into(),
        })
        .block_on_tokio()
        .unwrap();
}

/// Begins an interactive window resize.
///
/// This will start resizing the window under the pointer until `button` is released.
///
/// `button` should be the mouse button that is held at the time
/// this function is called. Otherwise, the move will not start.
/// This is intended for use in tandem with a mousebind.
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::window;
/// # use pinnacle_api::input;
/// # use pinnacle_api::input::Mod;
/// # use pinnacle_api::input::MouseButton;
/// input::mousebind(Mod::SUPER, MouseButton::Right)
///     .on_press(|| window::begin_resize(MouseButton::Right));
/// ```
pub fn begin_resize(button: MouseButton) {
    Client::window()
        .resize_grab(ResizeGrabRequest {
            button: button.into(),
        })
        .block_on_tokio()
        .unwrap();
}

/// Connects to a [`WindowSignal`].
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::window;
/// # use pinnacle_api::signal::WindowSignal;
/// window::connect_signal(WindowSignal::PointerEnter(Box::new(|window| {
///     window.set_focused(true);
/// })));
/// ```
pub fn connect_signal(signal: WindowSignal) -> SignalHandle {
    let mut signal_state = Client::signal_state();

    match signal {
        WindowSignal::PointerEnter(f) => signal_state.window_pointer_enter.add_callback(f),
        WindowSignal::PointerLeave(f) => signal_state.window_pointer_leave.add_callback(f),
        WindowSignal::Focused(f) => signal_state.window_focused.add_callback(f),
        WindowSignal::TitleChanged(f) => signal_state.window_title_changed.add_callback(f),
        WindowSignal::LayoutModeChanged(f) => {
            signal_state.window_layout_mode_changed.add_callback(f)
        }
        WindowSignal::Created(f) => signal_state.window_created.add_callback(f),
        WindowSignal::Destroyed(f) => signal_state.window_destroyed.add_callback(f),
    }
}

/// A handle to a window.
///
/// This allows you to manipulate the window and get its properties.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WindowHandle {
    pub(crate) id: u32,
}

/// A window's current layout mode.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum LayoutMode {
    /// The window is tiled.
    Tiled,
    /// The window is floating.
    Floating,
    /// The window is fullscreen.
    Fullscreen,
    /// The window is maximized.
    Maximized,
}

impl TryFrom<window::v1::LayoutMode> for LayoutMode {
    type Error = ();

    fn try_from(value: window::v1::LayoutMode) -> Result<Self, Self::Error> {
        match value {
            window::v1::LayoutMode::Unspecified => Err(()),
            window::v1::LayoutMode::Tiled => Ok(LayoutMode::Tiled),
            window::v1::LayoutMode::Floating => Ok(LayoutMode::Floating),
            window::v1::LayoutMode::Fullscreen => Ok(LayoutMode::Fullscreen),
            window::v1::LayoutMode::Maximized => Ok(LayoutMode::Maximized),
            // window::v1::LayoutMode::Spilled => Ok(LayoutMode::Floating),
        }
    }
}

/// A mode for window decorations (titlebar, shadows, etc).
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum DecorationMode {
    /// The client should draw its own decorations.
    ClientSide,
    /// The server should draw decorations.
    ServerSide,
}

/// A demand for variable refresh rate on an output.
#[derive(Default, Debug, Clone, Copy, Hash, PartialEq, Eq)]
#[non_exhaustive]
pub struct VrrDemand {
    /// Whether the window must be fullscreen for vrr to turn on.
    pub fullscreen: bool,
}

impl VrrDemand {
    /// Creates a [`VrrDemand`] that turns on vrr when a window is visible.
    pub fn when_visible() -> Self {
        Default::default()
    }

    /// Creates a [`VrrDemand`] that turns on vrr when a window is both
    /// visible *and* fullscreen.
    pub fn when_fullscreen() -> Self {
        Self { fullscreen: true }
    }
}

impl WindowHandle {
    /// Sends a close request to this window.
    ///
    /// If the window is unresponsive, it may not close.
    pub fn close(&self) {
        let window_id = self.id;
        Client::window()
            .close(pinnacle_api_defs::pinnacle::window::v1::CloseRequest { window_id })
            .block_on_tokio()
            .unwrap();
    }

    /// Sets this window's location and/or size.
    ///
    /// Only affects the floating geometry of windows. Tiled geometries are calculated
    /// using the current layout.
    pub fn set_geometry(
        &self,
        x: impl Into<Option<i32>>,
        y: impl Into<Option<i32>>,
        w: impl Into<Option<u32>>,
        h: impl Into<Option<u32>>,
    ) {
        Client::window()
            .set_geometry(SetGeometryRequest {
                window_id: self.id,
                x: x.into(),
                y: y.into(),
                w: w.into(),
                h: h.into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// If this window is tiled, resizes its tile by shifting the left, right,
    /// top, and bottom edges by the provided pixel amounts.
    ///
    /// Positive amounts shift edges right/down, while negative amounts
    /// shift edges left/up.
    ///
    /// If this resizes the tile in a direction that it can no longer resize
    /// towards (e.g. it's at the edge of the screen), it will resize in the opposite
    /// direction.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use pinnacle_api::window;
    /// # || {
    /// // Grow the focused tiled window 10 pixels leftward
    /// window::get_focused()?.resize_tile(-10, 0, 0, 0);
    ///
    /// // Shrink the focused tiled window 10 pixels inward from the right
    /// window::get_focused()?.resize_tile(0, -10, 0, 0);
    ///
    /// // Grow the focused tiled window 20 pixels centered vertically
    /// window::get_focused()?.resize_tile(0, 0, -10, 10);
    /// # Some(())
    /// # };
    /// ```
    pub fn resize_tile(&self, left: i32, right: i32, top: i32, bottom: i32) {
        Client::window()
            .resize_tile(ResizeTileRequest {
                window_id: self.id,
                left,
                right,
                top,
                bottom,
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Sets this window to fullscreen or not.
    pub fn set_fullscreen(&self, set: bool) {
        let window_id = self.id;
        Client::window()
            .set_fullscreen(SetFullscreenRequest {
                window_id,
                set_or_toggle: match set {
                    true => SetOrToggle::Set,
                    false => SetOrToggle::Unset,
                }
                .into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Toggles this window between fullscreen and not.
    pub fn toggle_fullscreen(&self) {
        let window_id = self.id;
        Client::window()
            .set_fullscreen(SetFullscreenRequest {
                window_id,
                set_or_toggle: SetOrToggle::Toggle.into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Sets this window to maximized or not.
    pub fn set_maximized(&self, set: bool) {
        let window_id = self.id;
        Client::window()
            .set_maximized(SetMaximizedRequest {
                window_id,
                set_or_toggle: match set {
                    true => SetOrToggle::Set,
                    false => SetOrToggle::Unset,
                }
                .into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Toggles this window between maximized and not.
    pub fn toggle_maximized(&self) {
        let window_id = self.id;
        Client::window()
            .set_maximized(SetMaximizedRequest {
                window_id,
                set_or_toggle: SetOrToggle::Toggle.into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Sets this window to floating or not.
    ///
    /// Floating windows will not be tiled and can be moved around and resized freely.
    pub fn set_floating(&self, set: bool) {
        let window_id = self.id;
        Client::window()
            .set_floating(SetFloatingRequest {
                window_id,
                set_or_toggle: match set {
                    true => SetOrToggle::Set,
                    false => SetOrToggle::Unset,
                }
                .into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Toggles this window to and from floating.
    ///
    /// Floating windows will not be tiled and can be moved around and resized freely.
    pub fn toggle_floating(&self) {
        let window_id = self.id;
        Client::window()
            .set_floating(SetFloatingRequest {
                window_id,
                set_or_toggle: SetOrToggle::Toggle.into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Focuses or unfocuses this window.
    pub fn set_focused(&self, set: bool) {
        let window_id = self.id;
        Client::window()
            .set_focused(SetFocusedRequest {
                window_id,
                set_or_toggle: match set {
                    true => SetOrToggle::Set,
                    false => SetOrToggle::Unset,
                }
                .into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Toggles this window between focused and unfocused.
    pub fn toggle_focused(&self) {
        let window_id = self.id;
        Client::window()
            .set_focused(SetFocusedRequest {
                window_id,
                set_or_toggle: SetOrToggle::Toggle.into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Sets this window's decoration mode.
    pub fn set_decoration_mode(&self, mode: DecorationMode) {
        Client::window()
            .set_decoration_mode(SetDecorationModeRequest {
                window_id: self.id,
                decoration_mode: match mode {
                    DecorationMode::ClientSide => window::v1::DecorationMode::ClientSide,
                    DecorationMode::ServerSide => window::v1::DecorationMode::ServerSide,
                }
                .into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Moves this window to the specified output.
    ///
    /// This will set the window workspaces to the output workspaces, and update the window position.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use pinnacle_api::window;
    /// # use pinnacle_api::output;
    /// # || {
    /// // Move the focused window to output DP-2
    /// window::get_focused()?.move_to_output(&output::get_by_name("DP-2")?);
    /// # Some(())
    /// # };
    /// ```
    pub fn move_to_output(&self, output: &OutputHandle) {
        let window_id = self.id;
        let output_name = output.name();

        Client::window()
            .move_to_output(MoveToOutputRequest {
                window_id,
                output_name,
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Moves this window to the given `workspace`.
    ///
    /// A window is always on exactly one workspace.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use pinnacle_api::window;
    /// # use pinnacle_api::workspace;
    /// # || {
    /// // Move the focused window to workspace "Code" on the focused output
    /// window::get_focused()?.move_to_workspace(&workspace::get("Code")?);
    /// # Some(())
    /// # };
    /// ```
    pub fn move_to_workspace(&self, workspace: &WorkspaceHandle) {
        let window_id = self.id;
        let workspace_id = workspace.id;
        Client::window()
            .move_to_workspace(MoveToWorkspaceRequest { window_id, workspace_id })
            .block_on_tokio()
            .unwrap();
    }

    /// Moves this window to `workspace` when `set` is true.
    ///
    /// A window is always on exactly one workspace, so unsetting does nothing.
    /// Prefer [`move_to_workspace`][Self::move_to_workspace].
    pub fn set_workspace(&self, workspace: &WorkspaceHandle, set: bool) {
        let window_id = self.id;
        let workspace_id = workspace.id;
        Client::window()
            .set_workspace(SetWorkspaceRequest {
                window_id,
                workspace_id,
                set_or_toggle: match set {
                    true => SetOrToggle::Set,
                    false => SetOrToggle::Unset,
                }
                .into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Moves this window to `workspace` if it isn't already there.
    ///
    /// A window is always on exactly one workspace, so this never removes it.
    /// Prefer [`move_to_workspace`][Self::move_to_workspace].
    pub fn toggle_workspace(&self, workspace: &WorkspaceHandle) {
        let window_id = self.id;
        let workspace_id = workspace.id;
        Client::window()
            .set_workspace(SetWorkspaceRequest {
                window_id,
                workspace_id,
                set_or_toggle: SetOrToggle::Toggle.into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Moves this window to the first of the given workspaces.
    ///
    /// A window is always on exactly one workspace, so any further workspaces are ignored.
    /// Passing in an empty collection does nothing.
    /// Prefer [`move_to_workspace`][Self::move_to_workspace].
    pub fn set_workspaces<T: Borrow<WorkspaceHandle>>(&self, workspaces: impl IntoIterator<Item = T>) {
        let window_id = self.id;
        let workspace_ids = workspaces
            .into_iter()
            .map(|workspace| {
                let workspace: &WorkspaceHandle = workspace.borrow();
                workspace.id
            })
            .collect::<Vec<_>>();

        Client::window()
            .set_workspaces(SetWorkspacesRequest { window_id, workspace_ids })
            .block_on_tokio()
            .unwrap();
    }

    /// Sets this window's [`VrrDemand`].
    ///
    /// When set to `None`, this window has no vrr demand.
    ///
    /// This works in conjunction with an output with
    /// [`Vrr::OnDemand`](crate::output::Vrr::OnDemand).
    pub fn set_vrr_demand(&self, vrr_demand: impl Into<Option<VrrDemand>>) {
        let window_id = self.id;
        let vrr_demand: Option<_> = vrr_demand.into();

        Client::window()
            .set_vrr_demand(SetVrrDemandRequest {
                window_id,
                vrr_demand: vrr_demand.map(|vrr_demand| window::v1::VrrDemand {
                    fullscreen: vrr_demand.fullscreen,
                }),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Raises this window to the front.
    pub fn raise(&self) {
        let window_id = self.id;
        Client::window()
            .raise(RaiseRequest { window_id })
            .block_on_tokio()
            .unwrap();
    }

    /// Lowers this window to the back.
    pub fn lower(&self) {
        let window_id = self.id;
        Client::window()
            .lower(LowerRequest { window_id })
            .block_on_tokio()
            .unwrap();
    }

    /// Gets this window's current location in the global space.
    pub fn loc(&self) -> Option<Point> {
        self.loc_async().block_on_tokio()
    }

    /// Async impl for [`Self::loc`].
    pub async fn loc_async(&self) -> Option<Point> {
        let window_id = self.id;
        Client::window()
            .get_loc(GetLocRequest { window_id })
            .await
            .unwrap()
            .into_inner()
            .loc
            .map(|loc| Point { x: loc.x, y: loc.y })
    }

    /// Gets this window's current size.
    pub fn size(&self) -> Option<Size> {
        self.size_async().block_on_tokio()
    }

    /// Async impl for [`Self::size`].
    pub async fn size_async(&self) -> Option<Size> {
        let window_id = self.id;
        Client::window()
            .get_size(GetSizeRequest { window_id })
            .await
            .unwrap()
            .into_inner()
            .size
            .map(|size| Size {
                w: size.width,
                h: size.height,
            })
    }

    /// Gets this window's app id (class if it's an xwayland window).
    ///
    /// If it doesn't have one, this returns an empty string.
    pub fn app_id(&self) -> String {
        self.app_id_async().block_on_tokio()
    }

    /// Async impl for [`Self::app_id`].
    pub async fn app_id_async(&self) -> String {
        let window_id = self.id;
        Client::window()
            .get_app_id(GetAppIdRequest { window_id })
            .await
            .unwrap()
            .into_inner()
            .app_id
    }

    /// Gets this window's title.
    ///
    /// If it doesn't have one, this returns an empty string.
    pub fn title(&self) -> String {
        self.title_async().block_on_tokio()
    }

    /// Async impl for [`Self::title`].
    pub async fn title_async(&self) -> String {
        let window_id = self.id;
        Client::window()
            .get_title(GetTitleRequest { window_id })
            .await
            .unwrap()
            .into_inner()
            .title
    }

    /// Gets this window's output.
    ///
    /// This is currently implemented as the output of the first
    /// workspace that this window has.
    ///
    /// Returns `None` if this window doesn't exist or if it has no workspaces.
    pub fn output(&self) -> Option<OutputHandle> {
        self.output_async().block_on_tokio()
    }

    /// Async impl for [`Self::output`].
    pub async fn output_async(&self) -> Option<OutputHandle> {
        Some(self.workspaces_async().await.next()?.output())
    }

    /// Gets whether or not this window has keyboard focus.
    pub fn focused(&self) -> bool {
        self.focused_async().block_on_tokio()
    }

    /// Async impl for [`Self::focused`].
    pub async fn focused_async(&self) -> bool {
        let window_id = self.id;
        Client::window()
            .get_focused(GetFocusedRequest { window_id })
            .await
            .unwrap()
            .into_inner()
            .focused
    }

    /// Gets this window's current [`LayoutMode`].
    pub fn layout_mode(&self) -> LayoutMode {
        self.layout_mode_async().block_on_tokio()
    }

    /// Async impl for [`Self::layout_mode`].
    pub async fn layout_mode_async(&self) -> LayoutMode {
        let window_id = self.id;
        Client::window()
            .get_layout_mode(GetLayoutModeRequest { window_id })
            .await
            .unwrap()
            .into_inner()
            .layout_mode()
            .try_into()
            .unwrap_or(LayoutMode::Tiled)
    }

    /// Gets whether or not this window is floating.
    pub fn floating(&self) -> bool {
        self.floating_async().block_on_tokio()
    }

    /// Async impl for [`Self::floating`].
    pub async fn floating_async(&self) -> bool {
        self.layout_mode_async().await == LayoutMode::Floating
    }

    /// Gets whether or not this window is tiled.
    pub fn tiled(&self) -> bool {
        self.tiled_async().block_on_tokio()
    }

    /// Async impl for [`Self::tiled`].
    pub async fn tiled_async(&self) -> bool {
        self.layout_mode_async().await == LayoutMode::Tiled
    }

    // /// Gets whether or not this window is spilled from the layout.
    // ///
    // /// A window is spilled when the current layout doesn't contains enough nodes and the
    // /// compositor cannot assign a geometry to it. In that state, the window behaves as a floating
    // /// window except that it gets tiled again if the number of nodes become big enough.
    // pub fn spilled(&self) -> bool {
    //     self.spilled_async().block_on_tokio()
    // }

    // /// Async impl for [`Self::spilled`].
    // pub async fn spilled_async(&self) -> bool {
    //     self.layout_mode_async().await == LayoutMode::Spilled
    // }

    /// Gets whether or not this window is fullscreen.
    pub fn fullscreen(&self) -> bool {
        self.fullscreen_async().block_on_tokio()
    }

    /// Async impl for [`Self::fullscreen`].
    pub async fn fullscreen_async(&self) -> bool {
        self.layout_mode_async().await == LayoutMode::Fullscreen
    }

    /// Gets whether or not this window is maximized.
    pub fn maximized(&self) -> bool {
        self.maximized_async().block_on_tokio()
    }

    /// Async impl for [`Self::maximized`].
    pub async fn maximized_async(&self) -> bool {
        self.layout_mode_async().await == LayoutMode::Maximized
    }

    /// Gets handles to all workspaces on this window.
    pub fn workspaces(&self) -> impl Iterator<Item = WorkspaceHandle> + use<> {
        self.workspaces_async().block_on_tokio()
    }

    /// Async impl for [`Self::workspaces`].
    pub async fn workspaces_async(&self) -> impl Iterator<Item = WorkspaceHandle> + use<> {
        let window_id = self.id;
        Client::window()
            .get_workspace_ids(GetWorkspaceIdsRequest { window_id })
            .await
            .unwrap()
            .into_inner()
            .workspace_ids
            .into_iter()
            .map(|id| WorkspaceHandle { id })
    }

    /// Gets whether or not this window has an active workspace.
    pub fn is_on_active_workspace(&self) -> bool {
        self.is_on_active_workspace_async().block_on_tokio()
    }

    /// Async impl for [`Self::is_on_active_workspace`].
    pub async fn is_on_active_workspace_async(&self) -> bool {
        self.workspaces_async()
            .await
            .batch_find(|workspace| workspace.active_async().boxed(), |active| *active)
            .is_some()
    }

    /// Gets all windows in the provided direction, sorted closest to farthest.
    pub fn in_direction(&self, direction: Direction) -> impl Iterator<Item = WindowHandle> + use<> {
        self.in_direction_async(direction).block_on_tokio()
    }

    /// Async impl for [`Self::in_direction`].
    pub async fn in_direction_async(
        &self,
        direction: Direction,
    ) -> impl Iterator<Item = WindowHandle> + use<> {
        let window_id = self.id;

        let mut request = GetWindowsInDirRequest {
            window_id,
            dir: Default::default(),
        };

        request.set_dir(match direction {
            Direction::Left => pinnacle_api_defs::pinnacle::util::v1::Dir::Left,
            Direction::Right => pinnacle_api_defs::pinnacle::util::v1::Dir::Right,
            Direction::Up => pinnacle_api_defs::pinnacle::util::v1::Dir::Up,
            Direction::Down => pinnacle_api_defs::pinnacle::util::v1::Dir::Down,
        });

        let response = Client::window()
            .get_windows_in_dir(request)
            .await
            .unwrap()
            .into_inner();

        response.window_ids.into_iter().map(WindowHandle::from_id)
    }

    /// Gets this window's ext-foreign-toplevel-list handle identifier.
    pub fn foreign_toplevel_list_identifier(&self) -> Option<String> {
        self.foreign_toplevel_list_identifier_async()
            .block_on_tokio()
    }

    /// Async impl for [`Self::foreign_toplevel_list_identifier`].
    pub async fn foreign_toplevel_list_identifier_async(&self) -> Option<String> {
        let window_id = self.id;

        Client::window()
            .get_foreign_toplevel_list_identifier(GetForeignToplevelListIdentifierRequest {
                window_id,
            })
            .await
            .unwrap()
            .into_inner()
            .identifier
    }

    /// Swap position with another window.
    pub fn swap(&self, target: &WindowHandle) {
        self.swap_async(target).block_on_tokio()
    }

    /// Async impl for [`Self::swap`].
    pub async fn swap_async(&self, target: &WindowHandle) {
        let request = SwapRequest {
            window_id: self.id,
            target_id: target.id,
        };

        Client::window().swap(request).await.unwrap();
    }

    /// Gets this window's raw compositor id.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Creates a window handle from an ID.
    ///
    /// Note: This is mostly for testing and if you want to serialize and deserialize window
    /// handles.
    pub fn from_id(id: u32) -> Self {
        Self { id }
    }
}

/// Adds a window rule.
///
/// Instead of using a declarative window rule system with match conditions,
/// you supply a closure that acts on a newly opened window.
/// You can use standard `if` statements and apply properties using the same
/// methods that are used everywhere else in this API.
///
/// Note: this function is special in that if it is called, Pinnacle will wait for
/// the provided closure to finish running before it sends windows an initial configure event.
/// *Do not block here*. At best, short blocks will increase the time it takes for a window to
/// open. At worst, a complete deadlock will prevent windows from opening at all.
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::window;
/// # use pinnacle_api::window::DecorationMode;
/// # use pinnacle_api::workspace;
/// window::add_window_rule(|window| {
///     // Make Alacritty always open on the "Terminal" workspace
///     if window.app_id() == "Alacritty" {
///         window.set_workspace(&workspace::get("Terminal").unwrap(), true);
///     }
///
///     // Make all windows use client-side decorations
///     window.set_decoration_mode(DecorationMode::ClientSide);
/// });
/// ```
pub fn add_window_rule(mut for_all: impl FnMut(WindowHandle) + Send + 'static) {
    let (client_outgoing, client_outgoing_to_server) = unbounded_channel();
    let client_outgoing_to_server =
        tokio_stream::wrappers::UnboundedReceiverStream::new(client_outgoing_to_server);
    let mut client_incoming = Client::window()
        .window_rule(client_outgoing_to_server)
        .block_on_tokio()
        .unwrap()
        .into_inner();

    let fut = async move {
        while let Some(Ok(response)) = client_incoming.next().await {
            let Some(response) = response.response else {
                continue;
            };

            match response {
                window::v1::window_rule_response::Response::NewWindow(new_window_request) => {
                    let request_id = new_window_request.request_id;
                    let window_id = new_window_request.window_id;

                    for_all(WindowHandle { id: window_id });

                    let sent = client_outgoing
                        .send(window::v1::WindowRuleRequest {
                            request: Some(window::v1::window_rule_request::Request::Finished(
                                window::v1::window_rule_request::Finished { request_id },
                            )),
                        })
                        .is_ok();

                    if !sent {
                        break;
                    }
                }
            }
        }
    };

    tokio::spawn(fut);
}
