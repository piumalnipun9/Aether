use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
};

use indexmap::IndexSet;
use mlua::{UserData, UserDataMethods};
use pinnacle::{state::WithState, workspace::Workspace};
use pinnacle_api::{layout::LayoutNode, output::OutputHandle, signal::WorkspaceSignal, workspace::WorkspaceHandle};
use proptest::prelude::*;
use smithay::{output::Output, utils::Rectangle};

use crate::{
    common::{Lang, fixture::Fixture, for_each_api},
    spawn_lua_blocking,
};

fn set_up() -> (Fixture, Output, Output, Vec<Workspace>, Vec<Workspace>) {
    let mut fixture = Fixture::new();

    let output1 = fixture.add_output(Rectangle::new((0, 0).into(), (1920, 1080).into()));
    output1.with_state_mut(|state| {
        let workspace = Workspace::new("1".to_string());
        workspace.set_active(true);
        let workspace2 = Workspace::new("2".to_string());
        let workspace3 = Workspace::new("3".to_string());
        state.add_workspaces([workspace, workspace2, workspace3]);
    });

    let output2 = fixture.add_output(Rectangle::new((1920, 0).into(), (1920, 1080).into()));
    output2.with_state_mut(|state| {
        let workspace = Workspace::new("4".to_string());
        workspace.set_active(true);
        let workspace2 = Workspace::new("5".to_string());
        let workspace3 = Workspace::new("6".to_string());
        state.add_workspaces([workspace, workspace2, workspace3]);
    });

    fixture.pinnacle().focus_output(&output1);

    fixture
        .runtime_handle()
        .block_on(pinnacle_api::connect())
        .unwrap();

    let workspaces1 = output1.with_state(|state| state.workspaces.clone());
    let workspaces2 = output2.with_state(|state| state.workspaces.clone());

    (
        fixture,
        output1,
        output2,
        workspaces1.into_iter().collect(),
        workspaces2.into_iter().collect(),
    )
}

#[test_log::test]
fn workspace_get_all() {
    let (mut fixture, ..) = set_up();

    fixture.spawn_blocking(|| {
        assert_eq!(pinnacle_api::workspace::get_all().count(), 6);
    });

    spawn_lua_blocking! {
        fixture,
        assert(#Workspace.get_all() == 6)
    }
}

#[test_log::test]
fn workspace_get() {
    let (mut fixture, _, output2, ..) = set_up();

    fixture.spawn_blocking({
        let output2_name = output2.name();
        move || {
            let workspace = pinnacle_api::workspace::get("1");
            assert!(workspace.is_some());

            let workspace = pinnacle_api::workspace::get("4");
            assert!(workspace.is_none());

            let workspace = pinnacle_api::workspace::get_on_output(
                "4",
                &pinnacle_api::output::get_by_name(&output2_name).unwrap(),
            );
            assert!(workspace.is_some());
        }
    });

    let output2_name = output2.name();
    spawn_lua_blocking! {
        fixture,
        local workspace = Workspace.get("1")
        assert(workspace)

        local workspace = Workspace.get("4")
        assert(not workspace)

        local workspace = Workspace.get("4", Output.get_by_name($output2_name))
        assert(workspace)
    }
}

#[test_log::test]
fn workspace_add() {
    for_each_api(|lang| {
        let (mut fixture, output, ..) = set_up();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                let workspaces = pinnacle_api::workspace::add(
                    &pinnacle_api::output::get_focused().unwrap(),
                    ["nubby's", "number", "factory"],
                );
                assert_eq!(workspaces.count(), 3);
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                local workspaces = Workspace.add(Output.get_focused(), "nubby's", "number", "factory")
                assert(#workspaces == 3)
            },
        }

        let workspace_count = output.with_state(|state| state.workspaces.len());
        assert_eq!(workspace_count, 6);
    });
}

#[test_log::test]
fn workspace_remove() {
    for_each_api(|lang| {
        let (mut fixture, output, ..) = set_up();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                let mut workspaces = pinnacle_api::output::get_focused().unwrap().workspaces();

                pinnacle_api::workspace::remove([workspaces.next().unwrap()]);
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                local workspaces = Output.get_focused():workspaces()
                Workspace.remove({ workspaces[1] })
            },
        }

        let workspace_count = output.with_state(|state| state.workspaces.len());
        assert_eq!(workspace_count, 2);
    });
}

/// Arguments for testing `workspace.move_to_output`.
#[derive(Debug, Clone)]
struct WorkspaceMoveToOutputArgs {
    /// A 2d map of workspaces that should be active.
    ///
    /// Each nested vec maps to one output.
    /// For each output, the length of the vec is the amount of workspaces
    /// the output will have. `true` will set that workspace active.
    workspace_actives: Vec<Vec<bool>>,
    /// The index of the output to focus.
    output_to_focus: usize,
    /// A 2d map of workspaces that will be moved to the target output.
    ///
    /// Similar to `workspace_actives`. `true` means "move this workspace".
    workspaces_to_move: Vec<Vec<bool>>,
    /// The index of the output that workspaces will be moved to.
    output_to_move_to: usize,
}

fn workspace_move_to_output_args() -> impl Strategy<Value = WorkspaceMoveToOutputArgs> {
    (2usize..=10)
        .prop_flat_map(|output_count| {
            (
                0..output_count,
                0..output_count,
                vec![2usize..=10; output_count],
            )
        })
        .prop_flat_map(|(output_to_focus, output_to_move_to, workspace_counts)| {
            (
                Just(output_to_focus),
                Just(output_to_move_to),
                workspace_counts
                    .iter()
                    .map(|&workspace_count| vec![proptest::bool::ANY; workspace_count])
                    .collect::<Vec<_>>(),
                workspace_counts
                    .iter()
                    .map(|&workspace_count| vec![proptest::bool::weighted(0.1); workspace_count])
                    .collect::<Vec<_>>(),
            )
        })
        .prop_map(
            |(output_to_focus, output_to_move_to, workspace_actives, workspaces_to_move)| WorkspaceMoveToOutputArgs {
                workspace_actives,
                output_to_focus,
                workspaces_to_move,
                output_to_move_to,
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 20,
        ..ProptestConfig::default()
    })]
    #[test_log::test]
    fn workspace_move_to_output(
        args in workspace_move_to_output_args()
    ) {
        for_each_api(|lang| {
            let mut fixture = Fixture::new();

            fixture
                .runtime_handle()
                .block_on(pinnacle_api::connect())
                .unwrap();

            let WorkspaceMoveToOutputArgs {
                workspace_actives,
                output_to_focus,
                workspaces_to_move,
                output_to_move_to
            } = args.clone();

            let mut outputs = Vec::new();
            let mut workspaces = Vec::new();
            let mut workspaces_to_mv = Vec::new();

            for (i, (workspace_mask, workspace_move)) in workspace_actives.clone().into_iter().zip(workspaces_to_move.clone()).enumerate() {
                let output = fixture.add_output(Rectangle::new((1920 * i as i32, 0).into(), (1920, 1080).into()));
                output.with_state_mut(|state| {
                    let tgs = workspace_mask
                        .into_iter()
                        .zip(workspace_move)
                        .map(|(active, r#move)| {
                            let workspace = Workspace::new("mama mia".to_string());
                            workspace.set_active(active);

                            if r#move {
                                workspaces_to_mv.push(workspace.clone());
                            }

                            workspace
                        })
                        .collect::<Vec<_>>();
                    state.add_workspaces(tgs.clone());
                    workspaces.push(tgs);
                });
                outputs.push(output);
            }

            fixture.pinnacle().focus_output(&outputs[output_to_focus]);

            if outputs[output_to_focus].with_state(|state| state.focused_workspaces().next().is_none()) {
                // Just ignore this case
                return;
            }

            let id = fixture.add_client();
            fixture.spawn_floating_window_with(id, (500, 500), |_| ());

            let window_has_moved_workspaces = !outputs[output_to_focus]
                .with_state(|state| {
                    state.focused_workspaces().cloned().collect::<IndexSet<_>>()
                })
                .is_disjoint(&workspaces_to_mv.iter().cloned().collect::<IndexSet<_>>());

            let target_output_name = outputs[output_to_move_to].name();
            let focused_output_name = outputs[output_to_focus].name();
            let workspace_ids_to_move = workspaces_to_mv
                .into_iter()
                .map(|workspace| workspace.id().to_inner())
                .collect::<Vec<_>>();

            match lang {
                Lang::Rust => fixture.spawn_blocking(move || {
                    let target_output = OutputHandle::from_name(target_output_name);
                    let focused_output = OutputHandle::from_name(focused_output_name);
                    let target_workspaces = workspace_ids_to_move
                        .into_iter()
                        .map(WorkspaceHandle::from_id)
                        .collect::<Vec<_>>();

                    let ret = pinnacle_api::workspace::move_to_output(&target_output, target_workspaces.clone());

                    match ret {
                        Ok(()) => {
                            let win = pinnacle_api::window::get_all().next().unwrap();

                            if window_has_moved_workspaces {
                                assert_eq!(win.output().as_ref(), Some(&target_output), "win not on target output");
                            } else {
                                assert_eq!(win.output().as_ref(), Some(&focused_output), "win not on focused output, moved");
                            }

                            for workspace in target_workspaces {
                                assert_eq!(workspace.output(), target_output, "workspace not on target output");
                            }
                        }
                        Err(err) => match err {
                            pinnacle_api::workspace::MoveToOutputError::OutputDoesNotExist => unreachable!(),
                            pinnacle_api::workspace::MoveToOutputError::SameWindowOnTwoOutputs(window_handles) => {
                                assert_eq!(window_handles.len(), 1, "win not on two ops");
                                let win = &window_handles[0];
                                assert_eq!(win.output().as_ref(), Some(&focused_output), "win not on focused output, not moved");
                            }
                        }
                    }
                }),
                Lang::Lua => spawn_lua_blocking! {
                    fixture,

                    local target_output = Output.handle.new($target_output_name)
                    local focused_output = Output.handle.new($focused_output_name)
                    local workspaces_to_move = Workspace.handle.new_from_table($workspace_ids_to_move)

                    local ok, err = Workspace.move_to_output(target_output, workspaces_to_move)

                    if ok then
                        assert(not err, "err was non-nil")
                        local win = Window.get_all()[1]
                        local win_output = win:output().name
                        if $window_has_moved_workspaces then
                            assert(win_output == target_output.name, "win not on target output")
                        else
                            assert(win_output == focused_output.name, "win not on focused output, moved")
                        end

                        for _, workspace in ipairs(workspaces_to_move) do
                            assert(workspace:output().name == target_output.name, "workspace not on target_output")
                        end
                    else
                        assert(not err.output_does_not_exist, "output doesn't exist")
                        assert(err.same_window_on_two_outputs, "window not on two ops")
                        local win = err.same_window_on_two_outputs[1]
                        assert(win:output().name == focused_output.name, "win not on focused output, not moved")
                    end
                }
            }
        });
    }
}

#[test_log::test]
fn workspace_handle_remove() {
    for_each_api(|lang| {
        let (mut fixture, output, ..) = set_up();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                let mut workspaces = pinnacle_api::output::get_focused().unwrap().workspaces();

                workspaces.next().unwrap().remove();
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                local workspaces = Output.get_focused():workspaces()
                workspaces[1]:remove()
            },
        }

        let workspace_count = output.with_state(|state| state.workspaces.len());
        assert_eq!(workspace_count, 2);
    });
}

#[test_log::test]
fn workspace_handle_switch_to() {
    for_each_api(|lang| {
        let (mut fixture, output, ..) = set_up();

        output.with_state(|state| {
            state.workspaces[0].set_active(true);
            state.workspaces[1].set_active(true);
            state.workspaces[2].set_active(true);
        });

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                pinnacle_api::workspace::get("2").unwrap().switch_to();
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                Workspace.get("2"):switch_to()
            },
        }

        output.with_state(|state| {
            assert!(!state.workspaces[0].active());
            assert!(state.workspaces[1].active());
            assert!(!state.workspaces[2].active());
        });
    });
}

#[test_log::test]
fn workspace_handle_set_active() {
    for_each_api(|lang| {
        let (mut fixture, output, ..) = set_up();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                pinnacle_api::workspace::get("1").unwrap().set_active(false);
                pinnacle_api::workspace::get("2").unwrap().set_active(true);
                pinnacle_api::workspace::get("3").unwrap().set_active(true);
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                Workspace.get("1"):set_active(false)
                Workspace.get("2"):set_active(true)
                Workspace.get("3"):set_active(true)
            },
        }

        output.with_state(|state| {
            assert!(!state.workspaces[0].active());
            assert!(state.workspaces[1].active());
            assert!(state.workspaces[2].active());
        });
    });
}

#[test_log::test]
fn workspace_handle_toggle_active() {
    for_each_api(|lang| {
        let (mut fixture, output, ..) = set_up();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                pinnacle_api::workspace::get("1").unwrap().toggle_active();
                pinnacle_api::workspace::get("2").unwrap().toggle_active();
                pinnacle_api::workspace::get("3").unwrap().toggle_active();
                pinnacle_api::workspace::get("3").unwrap().toggle_active();
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                Workspace.get("1"):toggle_active()
                Workspace.get("2"):toggle_active()
                Workspace.get("3"):toggle_active()
                Workspace.get("3"):toggle_active()
            },
        }

        output.with_state(|state| {
            assert!(!state.workspaces[0].active());
            assert!(state.workspaces[1].active());
            assert!(!state.workspaces[2].active());
        });
    });
}

#[test_log::test]
fn workspace_handle_active() {
    let (mut fixture, ..) = set_up();

    fixture.spawn_blocking(move || {
        assert!(pinnacle_api::workspace::get("1").unwrap().active());
        assert!(!pinnacle_api::workspace::get("2").unwrap().active());
        assert!(!pinnacle_api::workspace::get("3").unwrap().active());
    });

    spawn_lua_blocking! {
        fixture,
        assert(Workspace.get("1"):active())
        assert(not Workspace.get("2"):active())
        assert(not Workspace.get("3"):active())
    }
}

#[test_log::test]
fn workspace_handle_name() {
    let (mut fixture, ..) = set_up();

    fixture.spawn_blocking(move || {
        assert_eq!(pinnacle_api::workspace::get("1").unwrap().name(), "1");
        assert_eq!(pinnacle_api::workspace::get("2").unwrap().name(), "2");
        assert_eq!(pinnacle_api::workspace::get("3").unwrap().name(), "3");
    });

    spawn_lua_blocking! {
        fixture,
        assert(Workspace.get("1"):name() == "1")
        assert(Workspace.get("2"):name() == "2")
        assert(Workspace.get("3"):name() == "3")
    }
}

#[test_log::test]
fn workspace_handle_output() {
    let (mut fixture, output1, output2, ..) = set_up();

    fixture.spawn_blocking({
        let output1_name = output1.name();
        let output2_name = output2.name();
        move || {
            let workspace = pinnacle_api::workspace::get("1").unwrap();
            assert_eq!(workspace.output().name(), output1_name);

            let workspace = pinnacle_api::workspace::get_on_output(
                "4",
                &pinnacle_api::output::get_by_name(&output2_name).unwrap(),
            )
            .unwrap();
            assert_eq!(workspace.output().name(), output2_name);
        }
    });

    let output1_name = output1.name();
    let output2_name = output2.name();
    spawn_lua_blocking! {
        fixture,
        local workspace = Workspace.get("1")
        assert(workspace:output().name == $output1_name)

        local workspace = Workspace.get("4", Output.get_by_name($output2_name))
        assert(workspace:output().name == $output2_name)
    }
}

#[test_log::test]
fn workspace_handle_windows() {
    let (mut fixture, ..) = set_up();

    fixture.spawn_blocking(|| {
        pinnacle_api::layout::manage(|_| pinnacle_api::layout::LayoutResponse {
            root_node: LayoutNode::new(),
            tree_id: 0,
        })
    });

    let id = fixture.add_client();
    fixture.spawn_windows(1, id);

    fixture.spawn_blocking(move || {
        let workspace = pinnacle_api::workspace::get("1").unwrap();
        assert_eq!(workspace.windows().count(), 1);

        let workspace = pinnacle_api::workspace::get("2").unwrap();
        assert_eq!(workspace.windows().count(), 0);
    });

    spawn_lua_blocking! {
        fixture,
        local workspace = Workspace.get("1")
        assert(#workspace:windows() == 1)

        local workspace = Workspace.get("2")
        assert(#workspace:windows() == 0)
    }
}

#[test_log::test]
fn workspace_get_all_does_not_return_workspaces_cleared_after_config_reload() {
    for_each_api(|lang| {
        let (mut fixture, ..) = set_up();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                assert_eq!(pinnacle_api::workspace::get_all().count(), 6);
                pinnacle_api::pinnacle::reload_config();
                assert_eq!(pinnacle_api::workspace::get_all().count(), 0);
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                assert(#Workspace.get_all() == 6)
                Pinnacle.reload_config()
                assert(#Workspace.get_all() == 0)
            },
        }
    });
}

#[test_log::test]
fn workspace_get_does_not_return_workspaces_cleared_after_config_reload() {
    for_each_api(|lang| {
        let (mut fixture, ..) = set_up();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                assert!(pinnacle_api::workspace::get("1").is_some());
                pinnacle_api::pinnacle::reload_config();
                assert!(pinnacle_api::workspace::get("1").is_none());
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                assert(Workspace.get("1"))
                Pinnacle.reload_config()
                assert(not Workspace.get("1"))
            },
        }
    });
}

// TODO: Implement a less shady/more generic way to test signals,
// ideally something allowing to describe an expected sequence/list of signals
#[derive(Clone)]
struct WorkspaceSignalTester {
    active: Arc<Mutex<HashMap<WorkspaceHandle, bool>>>,
    created: Arc<Mutex<HashSet<WorkspaceHandle>>>,
    removed: Arc<Mutex<HashSet<WorkspaceHandle>>>,
    done: Arc<dyn Fn(&Self) -> bool + Send + Sync + 'static>,
}

impl WorkspaceSignalTester {
    fn new<F>(done: F) -> Self
    where
        F: Fn(&Self) -> bool + Send + Sync + 'static,
    {
        Self {
            active: Default::default(),
            created: Default::default(),
            removed: Default::default(),
            done: Arc::new(done),
        }
    }

    fn log_active(&self, workspace: WorkspaceHandle, active: bool) {
        let mut storage = self.active.lock().unwrap();
        storage.insert(workspace, active);
    }

    fn active(&self) -> &Arc<Mutex<HashMap<WorkspaceHandle, bool>>> {
        &self.active
    }

    fn log_created(&self, workspace: WorkspaceHandle) {
        let mut storage = self.created.lock().unwrap();
        storage.insert(workspace);
    }

    fn created(&self) -> &Arc<Mutex<HashSet<WorkspaceHandle>>> {
        &self.created
    }

    fn log_removed(&self, workspace: WorkspaceHandle) {
        let mut storage = self.removed.lock().unwrap();
        storage.insert(workspace);
    }

    fn removed(&self) -> &Arc<Mutex<HashSet<WorkspaceHandle>>> {
        &self.removed
    }

    fn done(&self) -> bool {
        (self.done)(&self.clone())
    }
}

impl UserData for WorkspaceSignalTester {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("log_active", |_, this, (id, active): (u32, bool)| {
            let handle = WorkspaceHandle::from_id(id);

            this.log_active(handle, active);

            Ok(())
        });

        methods.add_method("log_created", |_, this, id: u32| {
            let handle = WorkspaceHandle::from_id(id);

            this.log_created(handle);

            Ok(())
        });

        methods.add_method("log_removed", |_, this, id: u32| {
            let handle = WorkspaceHandle::from_id(id);

            this.log_removed(handle);

            Ok(())
        });

        methods.add_method("done", |_, this, ()| Ok(this.done()));
    }
}

#[test_log::test]
fn workspace_signal_active() {
    for_each_api(|lang| {
        let (mut fixture, _o1, _o2, workspaces, ..) = set_up();

        let workspace_name = workspaces[1].name();
        let handle = WorkspaceHandle::from_id(workspaces[1].id().to_inner());

        let tester = WorkspaceSignalTester::new(move |t| {
            let Ok(active) = t.active().try_lock() else {
                return false;
            };

            active.contains_key(&handle)
        });

        let workspace1_hndl = WorkspaceHandle::from_id(workspaces[0].id().to_inner());
        let workspace2_hndl = WorkspaceHandle::from_id(workspaces[1].id().to_inner());
        let tester_cpy = tester.clone();
        let signal_handle = Arc::new(OnceLock::new());
        let signal_handle_clone = signal_handle.clone();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                let handle = pinnacle_api::workspace::connect_signal(
                    pinnacle_api::signal::WorkspaceSignal::Active(Box::new(move |workspace, active| {
                        tester.log_active(workspace.clone(), active);
                    })),
                );
                signal_handle_clone.set(handle).unwrap();

                pinnacle_api::workspace::get(workspace_name).unwrap().switch_to();
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                Workspace.connect_signal({
                    active = function(workspace, active)
                        $tester:log_active(workspace.id, active)
                    end
                })

                Workspace.get($workspace_name):switch_to()

                local client = require("pinnacle.grpc.client").client
                while not $tester:done() do
                    client.loop:step();
                end
            },
        }

        fixture.dispatch_until(|_| tester_cpy.done());

        let store = tester_cpy.active().lock().unwrap();
        assert_eq!(store.get(&workspace1_hndl), Some(&false));
        assert_eq!(store.get(&workspace2_hndl), Some(&true));

        if lang == Lang::Rust {
            // Think the Rust client waits for signals forever, was only able
            // to reproduce in release mode though, strange
            signal_handle.get().unwrap().disconnect();
        }
    })
}

#[test_log::test]
fn workspace_signal_created() {
    for_each_api(|lang| {
        let (mut fixture, output, ..) = set_up();

        let workspace_name = "test_workspace";
        let tester = WorkspaceSignalTester::new(move |t| {
            let Ok(created) = t.created().try_lock() else {
                return false;
            };

            !created.is_empty()
        });

        let tester_cpy = tester.clone();
        let signal_handle = Arc::new(OnceLock::new());
        let signal_handle_clone = signal_handle.clone();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                let handle =
                    pinnacle_api::workspace::connect_signal(WorkspaceSignal::Created(Box::new(move |workspace| {
                        tester.log_created(workspace.clone());
                    })));
                signal_handle_clone.set(handle).unwrap();

                let output = pinnacle_api::output::get_focused().unwrap();
                let _ = pinnacle_api::workspace::add(&output, [workspace_name]);
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                Workspace.connect_signal({
                    created = function(workspace)
                        $tester:log_created(workspace.id)
                    end
                })

                local out = Output.get_focused()
                Workspace.add(out, $workspace_name)

                local client = require("pinnacle.grpc.client").client
                while not $tester:done() do
                    client.loop:step();
                end
            },
        }

        fixture.dispatch_until(|_| tester_cpy.done());

        let new_workspace = output
            .with_state(|s| {
                s.workspaces.iter().find_map(|t| {
                    if t.name() == workspace_name {
                        Some(WorkspaceHandle::from_id(t.id().to_inner()))
                    } else {
                        None
                    }
                })
            })
            .unwrap();

        let storage = tester_cpy.created().lock().unwrap();
        assert!(storage.contains(&new_workspace));

        if lang == Lang::Rust {
            signal_handle.get().unwrap().disconnect();
        }
    });
}

#[test_log::test]
fn workspace_signal_removed() {
    for_each_api(|lang| {
        let (mut fixture, _o1, _o2, workspaces, ..) = set_up();

        let workspace_name = "2";
        let workspace_handle = WorkspaceHandle::from_id(workspaces[1].id().to_inner());
        let tester = WorkspaceSignalTester::new(move |t| {
            let Ok(created) = t.removed().try_lock() else {
                return false;
            };

            !created.is_empty()
        });

        let tester_cpy = tester.clone();
        let signal_handle = Arc::new(OnceLock::new());
        let signal_handle_clone = signal_handle.clone();

        match lang {
            Lang::Rust => fixture.spawn_blocking(move || {
                let handle =
                    pinnacle_api::workspace::connect_signal(WorkspaceSignal::Removed(Box::new(move |workspace| {
                        tester.log_removed(workspace.clone());
                    })));
                signal_handle_clone.set(handle).unwrap();

                let to_remove = pinnacle_api::workspace::get(workspace_name).unwrap();
                pinnacle_api::workspace::remove([to_remove]);
            }),
            Lang::Lua => spawn_lua_blocking! {
                fixture,
                Workspace.connect_signal({
                    removed = function(workspace)
                        $tester:log_removed(workspace.id)
                    end
                })

                local to_remove = Workspace.get($workspace_name);
                Workspace.remove({to_remove})

                local client = require("pinnacle.grpc.client").client
                while not $tester:done() do
                    client.loop:step();
                end
            },
        }

        fixture.dispatch_until(|_| tester_cpy.done());

        let storage = tester_cpy.removed().lock().unwrap();
        assert!(storage.contains(&workspace_handle));

        if lang == Lang::Rust {
            signal_handle.get().unwrap().disconnect();
        }
    });
}
