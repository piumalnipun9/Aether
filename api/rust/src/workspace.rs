// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Workspace management.
//!
//! This module allows you to interact with Pinnacle's workspaces.
//!
//! # Workspaces
//! Workspaces work like they do in Hyprland or GNOME:
//!
//! - Every output has its own set of workspaces and always shows exactly one of them.
//! - Every window is on exactly one workspace and is shown only while that workspace is.
//!
//! Use [`WorkspaceHandle::switch_to`] to show a workspace and
//! [`WindowHandle::move_to_workspace`][crate::window::WindowHandle::move_to_workspace]
//! to move a window to one.

use futures::FutureExt;
use pinnacle_api_defs::pinnacle::{
    workspace::v1::{
        AddRequest, GetActiveRequest, GetNameRequest, GetOutputNameRequest, GetRequest,
        MoveToOutputRequest, RemoveRequest, SetActiveRequest, SwitchToRequest,
        move_to_output_response::error::Kind,
    },
    util::v1::SetOrToggle,
};

use crate::{
    BlockOnTokio,
    client::Client,
    output::OutputHandle,
    signal::{SignalHandle, WorkspaceSignal},
    util::Batch,
    window::WindowHandle,
};

/// Adds workspaces to the specified output.
///
/// This will add workspaces with the given names to `output` and return [`WorkspaceHandle`]s to all of
/// them.
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::output;
/// # use pinnacle_api::workspace;
/// // Add workspaces 1-5 to the focused output
/// if let Some(op) = output::get_focused() {
///     let workspaces = workspace::add(&op, ["1", "2", "3", "4", "5"]);
/// }
/// ```
pub fn add<I, T>(output: &OutputHandle, workspace_names: I) -> impl Iterator<Item = WorkspaceHandle> + use<I, T>
where
    I: IntoIterator<Item = T>,
    T: ToString,
{
    let output_name = output.name();
    let workspace_names = workspace_names.into_iter().map(|name| name.to_string()).collect();

    Client::workspace()
        .add(AddRequest {
            output_name,
            workspace_names,
        })
        .block_on_tokio()
        .unwrap()
        .into_inner()
        .workspace_ids
        .into_iter()
        .map(|id| WorkspaceHandle { id })
}

/// Gets handles to all workspaces across all outputs.
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::workspace;
/// for workspace in workspace::get_all() {
///     println!("{}", workspace.name());
/// }
/// ```
pub fn get_all() -> impl Iterator<Item = WorkspaceHandle> {
    get_all_async().block_on_tokio()
}

/// Async impl for [`get_all_async`].
pub async fn get_all_async() -> impl Iterator<Item = WorkspaceHandle> {
    Client::workspace()
        .get(GetRequest {})
        .await
        .unwrap()
        .into_inner()
        .workspace_ids
        .into_iter()
        .map(|id| WorkspaceHandle { id })
}

/// Gets a handle to the first workspace with the given `name` on the focused output.
///
/// To get the first workspace with the given `name` on a specific output, see
/// [`get_on_output`].
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::workspace;
/// # || {
/// let workspace = workspace::get("2")?;
/// # Some(())
/// # };
/// ```
pub fn get(name: impl ToString) -> Option<WorkspaceHandle> {
    get_async(name).block_on_tokio()
}

/// Async impl for [`get`].
pub async fn get_async(name: impl ToString) -> Option<WorkspaceHandle> {
    let name = name.to_string();
    let focused_op = crate::output::get_focused_async().await?;

    get_on_output_async(name, &focused_op).await
}

/// Gets a handle to the first workspace with the given `name` on `output`.
///
/// For a simpler way to get a workspace on the focused output, see [`get`].
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::output;
/// # use pinnacle_api::workspace;
/// # || {
/// let output = output::get_by_name("eDP-1")?;
/// let workspace = workspace::get_on_output("2", &output)?;
/// # Some(())
/// # };
/// ```
pub fn get_on_output(name: impl ToString, output: &OutputHandle) -> Option<WorkspaceHandle> {
    get_on_output_async(name, output).block_on_tokio()
}

/// Async impl for [`get_on_output`].
pub async fn get_on_output_async(name: impl ToString, output: &OutputHandle) -> Option<WorkspaceHandle> {
    let name = name.to_string();
    let output = output.clone();
    get_all_async().await.batch_find(
        |workspace| async { (workspace.name_async().await, workspace.output_async().await) }.boxed(),
        |(n, op)| *n == name && *op == output,
    )
}

/// Removes the given workspaces from their outputs.
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::workspace;
/// # use pinnacle_api::output;
/// # || {
/// let workspaces = workspace::add(&output::get_by_name("DP-1")?, ["1", "2", "Buckle", "Shoe"]);
///
/// workspace::remove(workspaces); // "DP-1" no longer has any workspaces
/// # Some(())
/// # };
/// ```
pub fn remove(workspaces: impl IntoIterator<Item = WorkspaceHandle>) {
    let workspace_ids = workspaces.into_iter().map(|handle| handle.id).collect::<Vec<_>>();

    Client::workspace()
        .remove(RemoveRequest { workspace_ids })
        .block_on_tokio()
        .unwrap();
}

/// Error that happens when moving workspaces to a different output.
#[derive(Debug, PartialEq, Clone)]
pub enum MoveToOutputError {
    /// The requested output to move the workspace to, does not exist
    OutputDoesNotExist,

    /// Moving the Workspace to another output would result in having the same window in multiple workspaces.
    /// It contains a list of windows that would be on multiple outputs.
    SameWindowOnTwoOutputs(Vec<WindowHandle>),
}

/// Moves existing workspaces to the specified output.
///
/// # Examples
///
/// ```no_run
/// # || {
/// # use pinnacle_api::output;
/// # use pinnacle_api::workspace;
/// let output = output::get_by_name("eDP-1")?;
/// let workspace_to_move = workspace::get("1")?;
/// workspace::move_to_output(&output, [workspace_to_move]);
/// # Some(())
/// # };
/// ```
pub fn move_to_output<I>(output: &OutputHandle, workspace_handles: I) -> Result<(), MoveToOutputError>
where
    I: IntoIterator<Item = WorkspaceHandle>,
{
    let output_name = output.name();
    let workspace_ids = workspace_handles.into_iter().map(|h| h.id).collect();

    let error = Client::workspace()
        .move_to_output(MoveToOutputRequest {
            output_name,
            workspace_ids,
        })
        .block_on_tokio()
        .unwrap()
        .into_inner()
        .error
        .and_then(|error| error.kind);

    match error {
        None => Ok(()),
        Some(Kind::OutputDoesNotExist(_)) => Err(MoveToOutputError::OutputDoesNotExist),
        Some(Kind::SameWindowOnTwoOutputs(windows)) => {
            Err(MoveToOutputError::SameWindowOnTwoOutputs(
                windows
                    .window_ids
                    .into_iter()
                    .map(WindowHandle::from_id)
                    .collect(),
            ))
        }
    }
}

/// Connects to a [`WorkspaceSignal`].
///
/// # Examples
///
/// ```no_run
/// # use pinnacle_api::workspace;
/// # use pinnacle_api::signal::WorkspaceSignal;
/// workspace::connect_signal(WorkspaceSignal::Active(Box::new(|workspace, active| {
///     println!("Workspace is active = {active}");
/// })));
/// ```
pub fn connect_signal(signal: WorkspaceSignal) -> SignalHandle {
    let mut signal_state = Client::signal_state();

    match signal {
        WorkspaceSignal::Active(f) => signal_state.workspace_active.add_callback(f),
        WorkspaceSignal::Created(f) => signal_state.workspace_created.add_callback(f),
        WorkspaceSignal::Removed(f) => signal_state.workspace_removed.add_callback(f),
    }
}

/// A handle to a workspace.
///
/// This handle allows you to do things like switch to workspaces and get their properties.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkspaceHandle {
    pub(crate) id: u32,
}

impl WorkspaceHandle {
    /// Creates a workspace handle from a numeric id.
    pub fn from_id(id: u32) -> Self {
        Self { id }
    }

    /// Shows this workspace on its output, hiding the one shown before.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use pinnacle_api::workspace;
    /// // Assume the focused output has the following inactive workspaces and windows:
    /// // "1": Alacritty
    /// // "2": Firefox, Discord
    /// // "3": Steam
    /// # || {
    /// workspace::get("2")?.switch_to(); // Displays Firefox and Discord
    /// workspace::get("3")?.switch_to(); // Displays Steam
    /// # Some(())
    /// # };
    /// ```
    pub fn switch_to(&self) {
        let workspace_id = self.id;

        Client::workspace()
            .switch_to(SwitchToRequest { workspace_id })
            .block_on_tokio()
            .unwrap();
    }

    /// Sets this workspace to active or not.
    ///
    /// An output always shows exactly one workspace, so `set_active(true)` is the same as
    /// [`switch_to`][Self::switch_to] and `set_active(false)` does nothing.
    pub fn set_active(&self, set: bool) {
        let workspace_id = self.id;

        Client::workspace()
            .set_active(SetActiveRequest {
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

    /// Switches to this workspace if it isn't shown.
    ///
    /// An output always shows exactly one workspace, so this does nothing when this
    /// workspace is already shown.
    pub fn toggle_active(&self) {
        let workspace_id = self.id;

        Client::workspace()
            .set_active(SetActiveRequest {
                workspace_id,
                set_or_toggle: SetOrToggle::Toggle.into(),
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Moves this workspace to the specified output.
    ///
    /// See [workspace::move_to_output][crate::workspace::move_to_output] for more information.
    pub fn move_to_output(&self, output: &OutputHandle) -> Result<(), MoveToOutputError> {
        move_to_output(output, [self.clone()])
    }

    /// Removes this workspace from its output.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use pinnacle_api::workspace;
    /// # use pinnacle_api::output;
    /// # || {
    /// let workspaces =
    ///     workspace::add(&output::get_by_name("DP-1")?, ["1", "2", "Buckle", "Shoe"]).collect::<Vec<_>>();
    ///
    /// workspaces[1].remove();
    /// workspaces[3].remove();
    /// # Some(())
    /// # };
    /// // "DP-1" now only has workspaces "1" and "Buckle"
    /// ```
    pub fn remove(&self) {
        let workspace_id = self.id;

        Client::workspace()
            .remove(RemoveRequest {
                workspace_ids: vec![workspace_id],
            })
            .block_on_tokio()
            .unwrap();
    }

    /// Gets whether or not this workspace is active.
    pub fn active(&self) -> bool {
        self.active_async().block_on_tokio()
    }

    /// Async impl for [`Self::active`].
    pub async fn active_async(&self) -> bool {
        let workspace_id = self.id;

        Client::workspace()
            .get_active(GetActiveRequest { workspace_id })
            .await
            .unwrap()
            .into_inner()
            .active
    }

    /// Gets this workspace's name.
    pub fn name(&self) -> String {
        self.name_async().block_on_tokio()
    }

    /// Async impl for [`Self::name`].
    pub async fn name_async(&self) -> String {
        let workspace_id = self.id;

        Client::workspace()
            .get_name(GetNameRequest { workspace_id })
            .await
            .unwrap()
            .into_inner()
            .name
    }

    /// Gets a handle to the output this workspace is on.
    pub fn output(&self) -> OutputHandle {
        self.output_async().block_on_tokio()
    }

    /// Async impl for [`Self::output`].
    pub async fn output_async(&self) -> OutputHandle {
        let workspace_id = self.id;

        let name = Client::workspace()
            .get_output_name(GetOutputNameRequest { workspace_id })
            .await
            .unwrap()
            .into_inner()
            .output_name;
        OutputHandle { name }
    }

    /// Gets all windows with this workspace.
    pub fn windows(&self) -> impl Iterator<Item = WindowHandle> + use<> {
        self.windows_async().block_on_tokio()
    }

    /// Async impl for [`Self::windows`].
    pub async fn windows_async(&self) -> impl Iterator<Item = WindowHandle> + use<> {
        let windows = crate::window::get_all_async().await;
        let this = self.clone();
        windows.batch_filter(
            |win| win.workspaces_async().boxed(),
            move |mut workspaces| workspaces.any(|workspace| workspace == this),
        )
    }

    /// Gets this workspace's raw compositor id.
    pub fn id(&self) -> u32 {
        self.id
    }
}
