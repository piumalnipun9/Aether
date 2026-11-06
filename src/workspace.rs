// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    fmt::Display,
    hash::Hash,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use smithay::output::Output;

use crate::state::{Pinnacle, WithState};

static WORKSPACE_ID_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A unique id for a [`Workspace`].
#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub struct WorkspaceId(u32);

impl Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl WorkspaceId {
    /// Creates a new workspace ID.
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    /// Get the next available `WorkspaceId`.
    fn next() -> Self {
        Self(WORKSPACE_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Get the workspace associated with this id.
    pub fn workspace(&self, pinnacle: &Pinnacle) -> Option<Workspace> {
        let _span = tracy_client::span!("WorkspaceId::workspace");

        pinnacle.outputs.iter().find_map(|op| {
            op.with_state(|state| {
                state
                    .workspaces
                    .iter()
                    // FIXME: a better workspace tracking system
                    .filter(|workspace| !workspace.defunct())
                    .find(|workspace| &workspace.id() == self)
                    .cloned()
            })
        })
    }

    /// Reset the global WorkspaceId counter.
    ///
    /// This is used, for example, when a config is reloaded and you want to keep
    /// windows on the same workspaces.
    pub fn reset() {
        WORKSPACE_ID_COUNTER.store(0, Ordering::SeqCst);
    }

    /// Gets the inner numeric ID.
    pub fn to_inner(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone)]
struct WorkspaceInner {
    /// The internal id of this workspace.
    id: WorkspaceId,
    /// The name of this workspace.
    name: String,
    /// Whether this workspace is active or not.
    active: bool,
    /// This workspace is defunct as a result of a config reload
    /// and will be replaced by the next added workspace.
    defunct: bool,
}

/// A marker for windows.
///
/// A window may have 0 or more workspaces, and you can display 0 or more workspaces
/// on each output at a time.
#[derive(Debug, Clone)]
pub struct Workspace {
    inner: Arc<Mutex<WorkspaceInner>>,
}

impl PartialEq for Workspace {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Workspace {}

impl Hash for Workspace {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let ptr = Arc::as_ptr(&self.inner);
        ptr.hash(state);
    }
}

// RefCell Safety: These methods should never panic because they are all self-contained or Copy.
impl Workspace {
    pub fn new(name: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WorkspaceInner {
                id: WorkspaceId::next(),
                name: name.clone(),
                active: false,
                defunct: false,
            })),
        }
    }

    /// Get the output this workspace is on.
    ///
    /// RefCell Safety: This uses RefCells on every mapped output.
    pub fn output(&self, pinnacle: &Pinnacle) -> Option<Output> {
        let _span = tracy_client::span!("Workspace::output");

        pinnacle
            .outputs
            .iter()
            .find(|output| output.with_state(|state| state.workspaces.contains(self)))
            .cloned()
    }

    /// Replace all inner fields of this workspace with ones from the `new_workspace`.
    pub fn replace(&self, new_workspace: Workspace) {
        let mut workspace = self.inner.lock().unwrap();
        *workspace = new_workspace.inner.lock().unwrap().clone();
        workspace.defunct = false;
    }

    /// Gets this workspace's unique numeric ID.
    pub fn id(&self) -> WorkspaceId {
        self.inner.lock().unwrap().id
    }

    /// Gets this workspace's name.
    pub fn name(&self) -> String {
        self.inner.lock().unwrap().name.clone()
    }

    /// Gets whether this workspace is active.
    pub fn active(&self) -> bool {
        self.inner.lock().unwrap().active
    }

    /// Sets this workspace's active state.
    ///
    /// Returns whether the new state is different from the old one,
    pub fn set_active(&self, active: bool) -> bool {
        std::mem::replace(&mut self.inner.lock().unwrap().active, active) != active
    }

    /// Gets whether this workspace is defunct as a result of a config reload.
    pub fn defunct(&self) -> bool {
        self.inner.lock().unwrap().defunct
    }

    /// Make this workspace defunct.
    pub fn make_defunct(&self) {
        self.inner.lock().unwrap().defunct = true;
    }
}
