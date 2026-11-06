// SPDX-License-Identifier: GPL-3.0-or-later

//! The app launcher and what it searches.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
};

use slint::{Image, Model, ModelRc, PlatformError, Rgba8Pixel, SharedPixelBuffer, VecModel};
use smithay::{
    input::keyboard::{KeysymHandle, keysyms},
    utils::{Logical, Point, Rectangle},
};

use super::{
    AppItem, BAR_HEIGHT, Launcher, ResultItem, Shell, ShellWindow, apps::App, search, set_platform,
    update_model,
};
use crate::render::util::blur::Blur;

/// The size the launcher tries to be in logical pixels.
const WIDTH: i32 = 660;
const HEIGHT: i32 = 640;

const MARGIN: i32 = 8;

/// How many of each thing a search shows.
const MAX_APPS: usize = 12;
const MAX_TOPICS: usize = 3;
const MAX_FILES: usize = 5;

/// `ResultItem.kind`.
const KIND_FILE: i32 = 0;
const KIND_WEB: i32 = 1;
const KIND_SUM: i32 = 2;
const KIND_SETTINGS: i32 = 3;

/// Sizes in the UI that arrow keys need for keeping things in view.
const CELL_HEIGHT: f32 = 96.0;
const SUGGESTIONS_HEIGHT: f32 = CELL_HEIGHT + 12.0;
const RESULT_HEIGHT: f32 = 56.0;
const SECTION_HEIGHT: f32 = 28.0;

/// What choosing something in the launcher does.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    App(String),
    /// A topic in the settings app.
    Settings(usize),
    File(PathBuf),
    Web(String),
    /// It's only there to be read.
    Nothing,
}

/// What was done in the launcher.
pub enum LauncherAction {
    Close,
    Query,
    Move { columns: i32, rows: i32 },
    Choose,
    ChooseApp(String),
    ChooseResult(usize),
}

/// The size of the launcher on an output of this size.
pub fn size(output: smithay::utils::Size<i32, Logical>) -> smithay::utils::Size<i32, Logical> {
    (
        WIDTH.min(output.w - MARGIN * 2),
        HEIGHT.min(output.h - BAR_HEIGHT - MARGIN * 2),
    )
        .into()
}

/// How far down an output it goes to be in the middle, while staying clear of the bar.
fn top(output_height: i32, height: i32) -> i32 {
    ((output_height - height) / 2).max(BAR_HEIGHT + MARGIN)
}

/// The launcher on an output.
pub struct OutputLauncher {
    pub ui: Launcher,
    pub window: ShellWindow,
    pub blur: RefCell<Blur>,
    apps: Rc<VecModel<AppItem>>,
    suggestions: Rc<VecModel<AppItem>>,
    results: Rc<VecModel<ResultItem>>,
    actions: Rc<RefCell<Vec<LauncherAction>>>,
    /// What each app and then each result does.
    targets: RefCell<Vec<Target>>,
    /// What's chosen with the keyboard, among `targets`.
    selected: Cell<Option<usize>>,
    images: RefCell<HashMap<String, Image>>,
    /// How far down the output it is, as it's in the middle of it.
    top: Cell<i32>,
    /// Which apps this was last brought up to date with.
    generation: Cell<u32>,
    outdated: Cell<bool>,
}

impl std::fmt::Debug for OutputLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputLauncher").finish_non_exhaustive()
    }
}

impl OutputLauncher {
    pub fn new(
        size: smithay::utils::Size<i32, Logical>,
        output_height: i32,
        scale: f64,
    ) -> Result<Self, PlatformError> {
        set_platform();
        let ui = Launcher::new()?;
        let window = ShellWindow::new(&ui, size, scale)?;

        let apps = Rc::new(VecModel::default());
        ui.set_apps(ModelRc::from(apps.clone()));
        let suggestions = Rc::new(VecModel::default());
        ui.set_suggestions(ModelRc::from(suggestions.clone()));
        let results = Rc::new(VecModel::default());
        ui.set_results(ModelRc::from(results.clone()));

        let actions = Rc::new(RefCell::new(Vec::new()));
        ui.on_query_changed({
            let actions = actions.clone();
            move |_| actions.borrow_mut().push(LauncherAction::Query)
        });
        ui.on_app_clicked({
            let actions = actions.clone();
            move |id| {
                actions
                    .borrow_mut()
                    .push(LauncherAction::ChooseApp(id.into()));
            }
        });
        ui.on_result_clicked({
            let actions = actions.clone();
            move |index| {
                actions
                    .borrow_mut()
                    .push(LauncherAction::ChooseResult(index as usize));
            }
        });

        ui.set_open(true);
        ui.invoke_focus_search();

        Ok(Self {
            ui,
            window,
            blur: Default::default(),
            apps,
            suggestions,
            results,
            actions,
            targets: RefCell::default(),
            selected: Cell::new(None),
            images: RefCell::default(),
            top: Cell::new(top(output_height, size.h)),
            generation: Cell::new(u32::MAX),
            outdated: Cell::new(true),
        })
    }

    pub fn take_actions(&self) -> Vec<LauncherAction> {
        std::mem::take(&mut self.actions.borrow_mut())
    }

    pub fn location(&self, output_width: i32) -> Point<i32, Logical> {
        ((output_width - self.window.size.w) / 2, self.top.get()).into()
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

    pub fn reopen(&self, output_height: i32) {
        self.top.set(top(output_height, self.window.size.h));
        self.ui.invoke_clear_search();
        self.ui.set_open(true);
        self.ui.invoke_focus_search();
        self.selected.set(None);
        self.outdated.set(true);
        self.ui.invoke_scroll_to(0.0, 0.0);
    }

    pub fn key(&self, key: &KeysymHandle<'_>, pressed: bool) {
        if !self.ui.get_open() {
            return;
        }

        let action = match key.modified_sym().raw() {
            keysyms::KEY_Escape => Some(LauncherAction::Close),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => Some(LauncherAction::Choose),
            keysyms::KEY_Up => Some(LauncherAction::Move {
                columns: 0,
                rows: -1,
            }),
            keysyms::KEY_Down => Some(LauncherAction::Move {
                columns: 0,
                rows: 1,
            }),
            keysyms::KEY_Left => Some(LauncherAction::Move {
                columns: -1,
                rows: 0,
            }),
            keysyms::KEY_Right => Some(LauncherAction::Move {
                columns: 1,
                rows: 0,
            }),
            _ => None,
        };

        match action {
            Some(action) if pressed => self.actions.borrow_mut().push(action),
            Some(_) => (),
            None => self.window.text_key(key, pressed),
        }
    }

    fn app_item(&self, app: &App, selected: bool) -> AppItem {
        let mut images = self.images.borrow_mut();

        if let Some(icon) = app.icon.as_ref().filter(|_| !images.contains_key(&app.id)) {
            let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                icon.as_raw(),
                icon.width(),
                icon.height(),
            );
            images.insert(app.id.clone(), Image::from_rgba8(buffer));
        }

        let icon = images.get(&app.id).cloned();
        AppItem {
            id: app.id.as_str().into(),
            name: app.name.as_str().into(),
            has_icon: icon.is_some(),
            icon: icon.unwrap_or_default(),
            initial: app
                .name
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default()
                .into(),
            selected,
        }
    }

    /// Asks for everything shown to be worked out again.
    pub fn invalidate(&self) {
        self.outdated.set(true);
    }

    pub fn update(&self, shell: &Shell) {
        if !self.outdated.get() && self.generation.get() == shell.apps_generation {
            return;
        }
        self.outdated.set(false);
        self.generation.set(shell.apps_generation);

        let query = self.ui.get_query().to_lowercase();
        let query = query.trim();

        let mut targets = Vec::new();
        let selected = self.selected.get();

        let apps: Vec<&App> = if query.is_empty() {
            shell.apps.iter().collect()
        } else {
            let mut scored = shell
                .apps
                .iter()
                .filter_map(|app| Some((search::app_score(app, query)?, app)))
                .collect::<Vec<_>>();
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
            scored
                .into_iter()
                .take(MAX_APPS)
                .map(|(_, app)| app)
                .collect()
        };

        let items = apps
            .iter()
            .enumerate()
            .map(|(index, app)| {
                targets.push(Target::App(app.id.clone()));
                self.app_item(app, selected == Some(index))
            })
            .collect();
        update_model(&self.apps, items);

        let suggestions = if query.is_empty() {
            let mut used = shell
                .apps
                .iter()
                .filter_map(|app| Some((*shell.launch_counts.get(&app.id)?, app)))
                .collect::<Vec<_>>();
            used.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
            used.into_iter()
                .take(self.ui.get_columns().max(1) as usize)
                .map(|(_, app)| self.app_item(app, false))
                .collect()
        } else {
            Vec::new()
        };
        update_model(&self.suggestions, suggestions);

        let mut results = Vec::new();
        let mut add = |section: &mut Option<&str>,
                       title: String,
                       subtitle: String,
                       kind: i32,
                       target: Target| {
            let index = targets.len();
            targets.push(target);
            results.push(ResultItem {
                section: section.take().unwrap_or_default().into(),
                title: title.into(),
                subtitle: subtitle.into(),
                kind,
                selected: selected == Some(index),
            });
        };

        if !query.is_empty() {
            let original = self.ui.get_query();

            if let Some(answer) = search::calculate(original.trim()) {
                add(
                    &mut Some("Calculator"),
                    answer,
                    original.trim().to_owned(),
                    KIND_SUM,
                    Target::Nothing,
                );
            }

            let mut section = Some("Settings");
            for topic in search::matching_topics(query).into_iter().take(MAX_TOPICS) {
                add(
                    &mut section,
                    topic.title.to_owned(),
                    topic.subtitle.to_owned(),
                    KIND_SETTINGS,
                    Target::Settings(topic.page),
                );
            }

            let mut section = Some("Files");
            let mut files = shell
                .files
                .iter()
                .filter_map(|path| {
                    let name = path.file_name()?.to_str()?;
                    Some((search::match_score(name, query)?, name, path))
                })
                .collect::<Vec<_>>();
            files.sort_by_key(|(score, _, _)| std::cmp::Reverse(*score));
            for (_, name, path) in files.into_iter().take(MAX_FILES) {
                add(
                    &mut section,
                    name.to_owned(),
                    path.parent()
                        .map(|parent| parent.display().to_string())
                        .unwrap_or_default(),
                    KIND_FILE,
                    Target::File(path.clone()),
                );
            }

            add(
                &mut Some("Web"),
                format!("Search the web for “{}”", original.trim()),
                String::new(),
                KIND_WEB,
                Target::Web(search::web_search_url(original.trim())),
            );
        }

        update_model(&self.results, results);
        *self.targets.borrow_mut() = targets;
    }

    /// Goes back to the top of what's found, as a new search starts over.
    pub fn on_query_changed(&self) {
        let searching = !self.ui.get_query().trim().is_empty();
        self.selected.set(searching.then_some(0));
        self.outdated.set(true);
        self.ui.invoke_scroll_to(0.0, 0.0);
    }

    fn scroll_to(&self, index: usize) {
        let columns = self.ui.get_columns().max(1) as usize;
        let apps = self.apps.row_count();
        let suggestions = if self.suggestions.row_count() > 0 {
            SUGGESTIONS_HEIGHT
        } else {
            0.0
        };

        let (top, height) = if index < apps {
            let row = (index / columns) as f32;
            (suggestions + row * CELL_HEIGHT, CELL_HEIGHT)
        } else {
            let rows = apps.div_ceil(columns) as f32 * CELL_HEIGHT;
            let before = (0..index - apps)
                .filter_map(|row| self.results.row_data(row))
                .map(|row| {
                    RESULT_HEIGHT + if row.section.is_empty() { 0.0 } else { SECTION_HEIGHT }
                })
                .sum::<f32>();
            (rows + before, RESULT_HEIGHT + SECTION_HEIGHT)
        };

        self.ui.invoke_scroll_to(top, height);
    }

    /// Moves what's chosen with the keyboard.
    pub fn move_selection(&self, columns: i32, rows: i32) {
        let count = self.targets.borrow().len();
        if count == 0 {
            return;
        }

        let next = self.selected.get().map_or(0, |current| {
            search::next_selection(
                current,
                columns,
                rows,
                self.apps.row_count(),
                count,
                self.ui.get_columns().max(1) as usize,
            )
        });

        self.selected.set(Some(next));
        self.outdated.set(true);
        self.scroll_to(next);
    }

    /// What's chosen with the keyboard.
    pub fn selected_target(&self) -> Option<Target> {
        self.targets.borrow().get(self.selected.get()?).cloned()
    }

    /// What the `index`th result does.
    pub fn result_target(&self, index: usize) -> Option<Target> {
        self.targets
            .borrow()
            .get(self.apps.row_count() + index)
            .cloned()
    }
}
