use crate::delegate_ext_workspace;
use crate::output::OutputName;
use crate::protocol::ext_workspace::{ExtWorkspaceHandler, ExtWorkspaceManagerState};
use crate::state::State;
use crate::workspace::WorkspaceId;

use smithay::output::Output;

impl ExtWorkspaceHandler for State {
    fn ext_workspace_manager_state(&mut self) -> &mut ExtWorkspaceManagerState {
        &mut self.pinnacle.ext_workspace_state
    }

    fn activate_workspace(&mut self, id: WorkspaceId) {
        if let Some(workspace) = id.workspace(&self.pinnacle) {
            crate::api::workspace::set_active(self, &workspace, Some(true));
        }
    }

    fn deactivate_workspace(&mut self, id: WorkspaceId) {
        if let Some(workspace) = id.workspace(&self.pinnacle) {
            crate::api::workspace::set_active(self, &workspace, Some(false));
        }
    }

    fn remove_workspace(&mut self, id: WorkspaceId) {
        crate::api::workspace::remove(self, Vec::from_iter(id.workspace(&self.pinnacle)));
    }

    fn add_workspace(&mut self, output: &Output, name: String) {
        crate::api::workspace::add(self, [name.clone()], OutputName(output.name()));
    }
}

delegate_ext_workspace!(State);
