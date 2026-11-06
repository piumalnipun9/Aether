use std::sync::Arc;
use std::sync::Mutex;

use pinnacle_api::input;
use pinnacle_api::input::Bind;
use pinnacle_api::input::Keysym;
use pinnacle_api::input::{Mod, MouseButton};
use pinnacle_api::layout;
use pinnacle_api::layout::LayoutGenerator;
use pinnacle_api::layout::LayoutNode;
use pinnacle_api::layout::LayoutResponse;
use pinnacle_api::layout::generators::Corner;
use pinnacle_api::layout::generators::CornerLocation;
use pinnacle_api::layout::generators::Cycle;
use pinnacle_api::layout::generators::Dwindle;
use pinnacle_api::layout::generators::Fair;
use pinnacle_api::layout::generators::Floating;
use pinnacle_api::layout::generators::MasterSide;
use pinnacle_api::layout::generators::MasterStack;
use pinnacle_api::layout::generators::Spiral;
use pinnacle_api::output;
use pinnacle_api::pinnacle;
use pinnacle_api::pinnacle::Backend;
use pinnacle_api::process::Command;
use pinnacle_api::signal::OutputSignal;
use pinnacle_api::signal::WindowSignal;
use pinnacle_api::workspace;
use pinnacle_api::util::{Axis, Batch};
use pinnacle_api::window;

async fn config() {
    // Change the mod key to `Alt` when running as a nested window.
    let mod_key = match pinnacle::backend() {
        Backend::Tty => Mod::SUPER,
        Backend::Window => Mod::ALT,
    };

    let terminal = "foot";

    //------------------------
    // Mousebinds            |
    //------------------------

    // `mod_key + left click` starts moving a window
    input::mousebind(mod_key, MouseButton::Left)
        .on_press(|| {
            window::begin_move(MouseButton::Left);
        })
        .group("Mouse")
        .description("Start an interactive window move");

    // `mod_key + right click` starts resizing a window
    input::mousebind(mod_key, MouseButton::Right)
        .on_press(|| {
            window::begin_resize(MouseButton::Right);
        })
        .group("Mouse")
        .description("Start an interactive window resize");

    //------------------------
    // Keybinds              |
    //------------------------

    // `mod_key + shift + q` quits Pinnacle
    input::keybind(mod_key | Mod::SHIFT, 'q')
        .set_as_quit()
        .group("Compositor")
        .description("Quit Pinnacle");


    // `mod_key + ctrl + r` reloads the config
    input::keybind(mod_key | Mod::CTRL, 'r')
        .set_as_reload_config()
        .group("Compositor")
        .description("Reload the config");

    // `mod_key + shift + c` closes the focused window
    input::keybind(mod_key | Mod::SHIFT, 'c')
        .on_press(|| {
            if let Some(window) = window::get_focused() {
                window.close();
            }
        })
        .group("Window")
        .description("Close the focused window");

    // `mod_key + Return` spawns a terminal
    input::keybind(mod_key, Keysym::Return)
        .on_press(move || {
            Command::new(terminal).spawn();
        })
        .group("Process")
        .description("Spawn a terminal");

    // `mod_key + ctrl + space` toggles floating
    input::keybind(mod_key | Mod::CTRL, Keysym::space)
        .on_press(|| {
            if let Some(window) = window::get_focused() {
                window.toggle_floating();
                window.raise();
            }
        })
        .group("Window")
        .description("Toggle floating on the focused window");

    // `mod_key + f` toggles fullscreen
    input::keybind(mod_key, 'f')
        .on_press(|| {
            if let Some(window) = window::get_focused() {
                window.toggle_fullscreen();
                window.raise();
            }
        })
        .group("Window")
        .description("Toggle fullscreen on the focused window");

    // `mod_key + m` toggles maximized
    input::keybind(mod_key, 'm')
        .on_press(|| {
            if let Some(window) = window::get_focused() {
                window.toggle_maximized();
                window.raise();
            }
        })
        .group("Window")
        .description("Toggle maximized on the focused window");

    // Media keybinds ------------------------------------------------------

    input::keybind(Mod::empty(), Keysym::XF86_AudioRaiseVolume)
        .on_press(|| {
            Command::new("wpctl")
                .args(["set-volume", "@DEFAULT_AUDIO_SINK@", "0.02+", "-l", "1.0"])
                .spawn();
        })
        .allow_when_locked()
        .group("Media")
        .description("Increase volume by 2%");

    input::keybind(Mod::empty(), Keysym::XF86_AudioLowerVolume)
        .on_press(|| {
            Command::new("wpctl")
                .args(["set-volume", "@DEFAULT_AUDIO_SINK@", "0.02-", "-l", "1.0"])
                .spawn();
        })
        .allow_when_locked()
        .group("Media")
        .description("Decrease volume by 2%");

    input::keybind(Mod::empty(), Keysym::XF86_AudioMute)
        .on_press(|| {
            Command::new("wpctl")
                .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
                .spawn();
        })
        .allow_when_locked()
        .group("Media")
        .description("Toggle mute");

    input::keybind(Mod::empty(), Keysym::XF86_AudioMicMute)
        .on_press(|| {
            Command::new("wpctl")
                .args(["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"])
                .spawn();
        })
        .allow_when_locked()
        .group("Media")
        .description("Toggle mic mute");

    input::keybind(Mod::empty(), Keysym::XF86_AudioPlay)
        .on_press(|| {
            Command::new("playerctl").arg("play-pause").spawn();
        })
        .allow_when_locked()
        .group("Media")
        .description("Play/pause media");

    input::keybind(Mod::empty(), Keysym::XF86_AudioStop)
        .on_press(|| {
            Command::new("playerctl").arg("stop").spawn();
        })
        .allow_when_locked()
        .group("Media")
        .description("Stop media");

    input::keybind(Mod::empty(), Keysym::XF86_AudioNext)
        .on_press(|| {
            Command::new("playerctl").arg("next").spawn();
        })
        .allow_when_locked()
        .group("Media")
        .description("Go to next media");

    input::keybind(Mod::empty(), Keysym::XF86_AudioPrev)
        .on_press(|| {
            Command::new("playerctl").arg("previous").spawn();
        })
        .allow_when_locked()
        .group("Media")
        .description("Go to previous media");

    // Display brightness keybinds ----------------------------------------

    input::keybind(Mod::empty(), Keysym::XF86_MonBrightnessUp)
        .on_press(|| {
            Command::new("brightnessctl")
                .args(["--class=backlight", "set", "+10%"])
                .spawn();
        })
        .allow_when_locked()
        .group("Display")
        .description("Increase display brightness by 10%");

    input::keybind(Mod::empty(), Keysym::XF86_MonBrightnessDown)
        .on_press(|| {
            Command::new("brightnessctl")
                .args(["--class=backlight", "set", "10%-"])
                .spawn();
        })
        .allow_when_locked()
        .group("Display")
        .description("Decrease display brightness by 10%");

    //------------------------
    // Layouts               |
    //------------------------

    // Pinnacle supports a tree-based layout system built on layout nodes.
    //
    // To determine the tree used to layout windows, Pinnacle requests your config for a tree data structure
    // with nodes containing gaps, directions, etc. There are a few provided utilities for creating
    // a layout, known as layout generators.
    //
    // ### Layout generators ###
    // A layout generator is a table that holds some state as well as
    // the `layout` function, which takes in a window count and computes
    // a tree of layout nodes that determines how windows are laid out.
    //
    // There are currently six built-in layout generators, one of which delegates to other
    // generators as shown below.

    fn into_box<'a, T: LayoutGenerator + Send + 'a>(
        generator: T,
    ) -> Box<dyn LayoutGenerator + Send + 'a> {
        Box::new(generator) as _
    }

    // Create a cycling layout generator that can cycle between layouts on different workspaces.
    let cycler = Arc::new(Mutex::new(Cycle::new([
        into_box(Dwindle::default()),
        into_box(MasterStack::default()),
        into_box(MasterStack {
            master_side: MasterSide::Right,
            ..Default::default()
        }),
        into_box(MasterStack {
            master_side: MasterSide::Top,
            ..Default::default()
        }),
        into_box(MasterStack {
            master_side: MasterSide::Bottom,
            ..Default::default()
        }),
        into_box(Spiral::default()),
        into_box(Corner::default()),
        into_box(Corner {
            corner_loc: CornerLocation::TopRight,
            ..Default::default()
        }),
        into_box(Corner {
            corner_loc: CornerLocation::BottomLeft,
            ..Default::default()
        }),
        into_box(Corner {
            corner_loc: CornerLocation::BottomRight,
            ..Default::default()
        }),
        into_box(Fair::default()),
        into_box(Fair {
            axis: Axis::Horizontal,
            ..Default::default()
        }),
        into_box(Floating::default()),
    ])));

    // Use the cycling layout generator to manage layout requests.
    // This returns a layout requester that allows you to request layouts manually.
    let layout_requester = layout::manage({
        let cycler = cycler.clone();
        move |args| {
            let Some(workspace) = args.workspaces.first() else {
                return LayoutResponse {
                    root_node: LayoutNode::new(),
                    tree_id: 0,
                };
            };

            let mut cycler = cycler.lock().unwrap();
            cycler.set_current_workspace(workspace.clone());

            let root_node = cycler.layout(args.window_count);
            let tree_id = cycler.current_tree_id();
            LayoutResponse { root_node, tree_id }
        }
    });

    // `mod_key + space` cycles to the next layout
    input::keybind(mod_key, Keysym::space)
        .on_press({
            let cycler = cycler.clone();
            let requester = layout_requester.clone();
            move || {
                let Some(focused_op) = output::get_focused() else {
                    return;
                };
                let Some(first_active_workspace) = focused_op
                    .workspaces()
                    .batch_find(|workspace| Box::pin(workspace.active_async()), |active| *active)
                else {
                    return;
                };

                cycler
                    .lock()
                    .unwrap()
                    .cycle_layout_forward(&first_active_workspace);
                requester.request_layout_on_output(&focused_op);
            }
        })
        .group("Layout")
        .description("Cycle the layout forward");

    // `mod_key + shift + space` cycles to the previous layout
    input::keybind(mod_key | Mod::SHIFT, Keysym::space)
        .on_press(move || {
            let Some(focused_op) = output::get_focused() else {
                return;
            };
            let Some(first_active_workspace) = focused_op
                .workspaces()
                .batch_find(|workspace| Box::pin(workspace.active_async()), |active| *active)
            else {
                return;
            };

            cycler
                .lock()
                .unwrap()
                .cycle_layout_backward(&first_active_workspace);
            layout_requester.request_layout_on_output(&focused_op);
        })
        .group("Layout")
        .description("Cycle the layout backward");

    //------------------------
    // Workspaces                  |
    //------------------------

    let workspace_names = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

    // Setup all monitors with workspaces "1" through "9"
    output::for_each_output(move |output| {
        let mut workspaces = workspace::add(output, workspace_names);
        workspaces.next().unwrap().set_active(true);
    });

    for workspace_name in workspace_names {
        // `mod_key + 1-9` switches to workspace "1" to "9"
        input::keybind(mod_key, workspace_name)
            .on_press(move || {
                if let Some(workspace) = workspace::get(workspace_name) {
                    workspace.switch_to();
                }
            })
            .group("Workspace")
            .description(format!("Switch to workspace {workspace_name}"));

        // `mod_key + ctrl + 1-9` toggles workspace "1" to "9"
        input::keybind(mod_key | Mod::CTRL, workspace_name)
            .on_press(move || {
                if let Some(workspace) = workspace::get(workspace_name) {
                    workspace.toggle_active();
                }
            })
            .group("Workspace")
            .description(format!("Toggle workspace {workspace_name}"));

        // `mod_key + shift + 1-9` moves the focused window to workspace "1" to "9"
        input::keybind(mod_key | Mod::SHIFT, workspace_name)
            .on_press(move || {
                if let Some(workspace) = workspace::get(workspace_name)
                    && let Some(win) = window::get_focused()
                {
                    win.move_to_workspace(&workspace);
                }
            })
            .group("Workspace")
            .description(format!("Move the focused window to workspace {workspace_name}"));

        // `mod_key + ctrl + shift + 1-9` toggles workspace "1" to "9" on the focused window
        input::keybind(mod_key | Mod::CTRL | Mod::SHIFT, workspace_name)
            .on_press(move || {
                if let Some(tg) = workspace::get(workspace_name)
                    && let Some(win) = window::get_focused()
                {
                    win.toggle_workspace(&tg);
                }
            })
            .group("Workspace")
            .description(format!("Toggle workspace {workspace_name} on the focused window"));
    }

    input::libinput::for_each_device(|device| {
        // Enable natural scroll for touchpads
        if device.device_type().is_touchpad() {
            device.set_natural_scroll(true);
        }
    });

    // Enable focus borders with titlebars

    // Enable sloppy focus
    window::connect_signal(WindowSignal::PointerEnter(Box::new(|win| {
        win.set_focused(true);
    })));

    // Focus outputs when the pointer enters them
    output::connect_signal(OutputSignal::PointerEnter(Box::new(|output| {
        output.focus();
    })));


}

pinnacle_api::main!(config);
