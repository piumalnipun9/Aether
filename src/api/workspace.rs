pub mod v1;

use std::mem;

use indexmap::IndexSet;
use smithay::output::Output;
use tracing::warn;

use crate::{
    output::OutputName,
    state::{State, WithState},
    workspace::Workspace,
    window::{UnmappedState, window_state::WindowId},
};

use super::{StateFnSender, signal::Signal};

pub struct WorkspaceService {
    sender: StateFnSender,
}

impl WorkspaceService {
    pub fn new(sender: StateFnSender) -> Self {
        Self { sender }
    }
}

/// Sets a workspace's active state.
///
/// Every output always shows exactly one workspace, so activating a workspace
/// switches to it, and deactivating the shown workspace does nothing.
pub fn set_active(state: &mut State, workspace: &Workspace, set: Option<bool>) {
    let active = set.unwrap_or(!workspace.active());

    if active {
        switch_to(state, workspace);
    }
}

/// Makes sure `output` shows exactly one workspace.
///
/// If several are active, `preferred` (when it is one of them) or the first active
/// workspace stays shown. If none are active, the first workspace is shown.
pub fn normalize_active(state: &mut State, output: &Output, preferred: Option<&Workspace>) {
    let workspaces = output.with_state(|op_state| op_state.workspaces.clone());

    let active = workspaces.iter().filter(|ws| ws.active()).collect::<Vec<_>>();
    let keep = match active.as_slice() {
        [] => workspaces.first(),
        [_] => return,
        many => preferred
            .filter(|ws| many.contains(ws))
            .or_else(|| many.first().copied()),
    };

    let Some(keep) = keep.cloned() else {
        return;
    };

    for ws in workspaces.iter() {
        if ws.set_active(*ws == keep) {
            state.pinnacle.signal_state.workspace_active.signal(ws);
        }
    }

    state.pinnacle.update_xwayland_stacking_order();
    state.pinnacle.request_layout(output);
    state.schedule_render(output);
}

pub fn switch_to(state: &mut State, workspace: &Workspace) {
    let Some(output) = workspace.output(&state.pinnacle) else {
        return;
    };

    if workspace.active() {
        return;
    }

    start_workspace_switch_animation(state, &output, workspace);

    output.with_state(|op_state| {
        for op_workspace in op_state.workspaces.iter() {
            if op_workspace.set_active(false) {
                state.pinnacle.signal_state.workspace_active.signal(op_workspace);
            }
        }
        if workspace.set_active(true) {
            state.pinnacle.signal_state.workspace_active.signal(workspace);
        }
    });

    state.pinnacle.update_xwayland_stacking_order();

    state.pinnacle.request_layout(&output);

    state.schedule_render(&output);
}

/// Slides the shown workspace out and `to` in, in the direction of its position
/// in the output's workspace list.
///
/// Call this before changing which workspace is active.
fn start_workspace_switch_animation(state: &mut State, output: &Output, to: &Workspace) {
    let (from_idx, to_idx) = output.with_state(|op_state| {
        (
            op_state.workspaces.iter().position(|ws| ws.active()),
            op_state.workspaces.get_index_of(to),
        )
    });
    let (Some(from_idx), Some(to_idx)) = (from_idx, to_idx) else {
        return;
    };

    let output_loc = output.current_location();
    let scale = output.current_scale().fractional_scale().into();

    // Snapshot what's shown now, bottom to top, before the new layout replaces it.
    let shown = state
        .pinnacle
        .space
        .elements_for_output(output)
        .filter_map(|win| Some((win.clone(), state.pinnacle.space.element_location(win)?)))
        .collect::<Vec<_>>();

    let mut old_windows = Vec::new();
    state.backend.with_renderer(|renderer| {
        for (win, loc) in shown {
            if let Some(snapshot) = win.capture_snapshot(renderer, scale) {
                old_windows.push((snapshot, loc - output_loc));
            }
        }
    });

    tracing::debug!(
        from_idx,
        to_idx,
        windows = old_windows.len(),
        "workspace switch animation started"
    );
    output.with_state_mut(|op_state| {
        op_state.workspace_switch = Some(crate::animation::WorkspaceSwitch {
            anim: crate::animation::Animation::new(
                crate::animation::duration::WORKSPACE,
                crate::animation::Curve::STANDARD,
            ),
            direction: if to_idx > from_idx { 1.0 } else { -1.0 },
            old_windows,
        });
    });
}

pub fn add(
    state: &mut State,
    workspace_names: impl IntoIterator<Item = String>,
    output_name: OutputName,
) -> Vec<Workspace> {
    let Some(output) = output_name.output(&state.pinnacle) else {
        warn!(
            "Tried to add workspaces to output {} but it doesn't exist",
            output_name.0
        );
        return Vec::new();
    };

    let new_workspaces = workspace_names.into_iter().map(Workspace::new).collect::<Vec<_>>();

    output.with_state_mut(|state| {
        state.add_workspaces(new_workspaces.clone());
    });

    if !new_workspaces.is_empty() {
        let mut unmapped_windows = mem::take(&mut state.pinnacle.unmapped_windows);
        for unmapped in unmapped_windows.iter_mut() {
            if !matches!(unmapped.state, UnmappedState::WaitingForWorkspaces { .. }) {
                continue;
            };

            unmapped.window.with_state_mut(|state| {
                state.workspaces = new_workspaces.first().cloned().into_iter().collect();
            });

            state.pinnacle.request_window_rules(unmapped);
        }
        state.pinnacle.unmapped_windows = unmapped_windows;
    }

    state.pinnacle.update_xwayland_stacking_order();

    for workspace in new_workspaces.iter() {
        state.pinnacle.signal_state.workspace_created.signal(workspace);
    }

    normalize_active(state, &output, None);

    new_workspaces
}

pub fn remove(state: &mut State, workspaces_to_remove: Vec<Workspace>) {
    for window in state.pinnacle.windows.iter() {
        window.with_state_mut(|state| {
            for workspace_to_remove in workspaces_to_remove.iter() {
                state.workspaces.shift_remove(workspace_to_remove);
            }
        })
    }

    for output in state.pinnacle.outputs.clone() {
        let mut changed = false;

        output.with_state_mut(|state| {
            for workspace_to_remove in workspaces_to_remove.iter() {
                changed = state.workspaces.shift_remove(workspace_to_remove) || changed;
            }
        });

        if changed {
            state.pinnacle.request_layout(&output);
            state.schedule_render(&output);
            normalize_active(state, &output, None);
        }
    }

    for conn_saved_state in state.pinnacle.config.connector_saved_states.values_mut() {
        for workspace_to_remove in workspaces_to_remove.iter() {
            conn_saved_state.workspaces.shift_remove(workspace_to_remove);
        }
    }

    state.pinnacle.update_xwayland_stacking_order();

    for workspace_to_remove in workspaces_to_remove.iter() {
        state
            .pinnacle
            .signal_state
            .workspace_removed
            .signal(workspace_to_remove);
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum WorkspaceMoveToOutputError {
    /// Its impossible to move workspaces to an output that does not exist. Create it first
    OutputDoesNotExist,
    /// Moving the task would result in a situation where each of the following windows are on multiple outputs. This would be an invalid state for pinnacle.
    SameWindowOnTwoOutputs(Vec<WindowId>),
}

pub fn move_to_output<T>(
    state: &mut State,
    workspaces_to_move: T,
    output_name: OutputName,
) -> Result<(), WorkspaceMoveToOutputError>
where
    T: IntoIterator<Item = Workspace>,
{
    let Some(new_output) = output_name.output(&state.pinnacle) else {
        return Err(WorkspaceMoveToOutputError::OutputDoesNotExist);
    };

    let workspaces_to_move: IndexSet<Workspace> = workspaces_to_move.into_iter().collect();
    let workspaces_on_other_outputs = state
        .pinnacle
        .outputs
        .iter()
        .filter(|output| **output != new_output)
        .flat_map(|output| output.with_state(|state| state.workspaces.clone()))
        .filter(|workspace| !workspaces_to_move.contains(workspace))
        .collect::<IndexSet<_>>();

    let mut problematic_windows = IndexSet::new();
    let mut windows_to_update = Vec::new();

    for window in state.pinnacle.windows.iter() {
        let (window_id, is_affected_by_move, has_other_output_workspace) = window.with_state(|state| {
            let is_affected_by_move = !state.workspaces.is_disjoint(&workspaces_to_move);
            let has_other_output_workspace = !state.workspaces.is_disjoint(&workspaces_on_other_outputs);

            (state.id, is_affected_by_move, has_other_output_workspace)
        });

        if is_affected_by_move {
            if has_other_output_workspace {
                problematic_windows.insert(window_id);
            }
            windows_to_update.push(window.clone());
        }
    }

    if !problematic_windows.is_empty() {
        return Err(WorkspaceMoveToOutputError::SameWindowOnTwoOutputs(
            problematic_windows.into_iter().collect(),
        ));
    }

    for output in state.pinnacle.outputs.clone() {
        let mut changed = false;

        if output.name() == new_output.name() {
            output.with_state_mut(|state| {
                for workspace in workspaces_to_move.iter() {
                    changed = state.workspaces.insert(workspace.clone()) || changed;
                }
            });
        } else {
            output.with_state_mut(|state| {
                for workspace in workspaces_to_move.iter() {
                    changed = state.workspaces.shift_remove(workspace) || changed;
                }
            });
        }

        if changed {
            state.pinnacle.request_layout(&output);
            state.schedule_render(&output);

            // A moved workspace that was shown stays shown on its new output.
            let preferred = workspaces_to_move.iter().find(|ws| ws.active()).cloned();
            normalize_active(state, &output, preferred.as_ref());
        }
    }

    if state.pinnacle.space.output_geometry(&new_output).is_some() {
        let loc = state.pinnacle.floating_loc_for_output(&new_output);
        for window in windows_to_update {
            window.with_state_mut(|state| state.set_floating_loc(Some(loc)));

            let layout_mode = window.with_state(|state| state.layout_mode);
            state.pinnacle.update_window_geometry(
                &window,
                layout_mode.is_tiled() || layout_mode.is_spilled(),
            );
        }
    }

    state.pinnacle.update_xwayland_stacking_order();
    Ok(())
}
