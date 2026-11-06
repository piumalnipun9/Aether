// SPDX-License-Identifier: GPL-3.0-or-later

//! Finding installed apps and their icons.

use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use image::{ImageReader, RgbaImage, imageops};
use resvg::{tiny_skia, usvg};
use tracing::warn;

/// The size of icons in pixels. They're shown at about half of this.
const ICON_SIZE: u32 = 96;

/// Icon themes to look in, after the one in use, for what it doesn't have.
const FALLBACK_THEMES: [&str; 2] = ["hicolor", "Adwaita"];

/// An installed app.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    /// The name of its desktop file.
    pub id: String,
    pub name: String,
    pub comment: String,
    pub keywords: Vec<String>,
    /// The command to run, as a shell command.
    pub command: String,
    pub terminal: bool,
    pub icon_name: String,
    pub icon: Option<Arc<RgbaImage>>,
}

/// Removes the codes in an `Exec` line that stand for files to open.
fn strip_field_codes(exec: &str) -> String {
    let mut command = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();

    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('%', Some('%')) => {
                command.push('%');
                chars.next();
            }
            ('%', Some(_)) => {
                chars.next();
            }
            _ => command.push(c),
        }
    }

    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parses a desktop file. Apps that aren't meant to be shown come out as `None`.
fn parse_desktop_entry(id: &str, contents: &str, desktop: &str) -> Option<App> {
    let mut fields = HashMap::new();
    let mut in_entry = false;

    for line in contents.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry && let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim(), value.trim());
        }
    }

    let flag = |key: &str| fields.get(key).is_some_and(|value| *value == "true");
    let list = |key: &str| {
        fields
            .get(key)
            .map(|value| {
                value
                    .split(';')
                    .filter(|item| !item.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    if fields
        .get("Type")
        .is_some_and(|kind| *kind != "Application")
        || flag("NoDisplay")
        || flag("Hidden")
    {
        return None;
    }

    let shown_in = list("OnlyShowIn");
    if !shown_in.is_empty() && !shown_in.iter().any(|name| name == desktop) {
        return None;
    }
    if list("NotShowIn").iter().any(|name| name == desktop) {
        return None;
    }

    Some(App {
        id: id.to_owned(),
        name: (*fields.get("Name")?).to_owned(),
        comment: fields
            .get("Comment")
            .copied()
            .unwrap_or_default()
            .to_owned(),
        keywords: list("Keywords"),
        command: strip_field_codes(fields.get("Exec")?),
        terminal: flag("Terminal"),
        icon_name: fields.get("Icon").copied().unwrap_or_default().to_owned(),
        icon: None,
    })
}

/// Where apps and icons are, with what's the user's own first.
fn data_dirs() -> Vec<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs = vec![
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share")),
    ];

    let system =
        env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    dirs.extend(
        system
            .split(':')
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from),
    );

    dirs
}

/// Reads the apps, sorted by name. Earlier directories win, as the specification says.
fn read_apps(desktop: &str) -> Vec<App> {
    let mut apps = HashMap::new();

    for dir in data_dirs() {
        let Ok(entries) = fs::read_dir(dir.join("applications")) else {
            continue;
        };

        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = id.strip_suffix(".desktop") else {
                continue;
            };
            if apps.contains_key(id) {
                continue;
            }

            // Being hidden here also hides what's further down, so it's kept for now.
            let app = fs::read_to_string(entry.path())
                .ok()
                .map(|contents| parse_desktop_entry(id, &contents, desktop));
            if let Some(app) = app {
                apps.insert(id.to_owned(), app);
            }
        }
    }

    let mut apps = apps.into_values().flatten().collect::<Vec<_>>();
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

/// How well an icon file suits us, which is higher the better.
fn icon_score(path: &Path) -> Option<u32> {
    let extension = path.extension()?.to_str()?;
    let dir = path.parent()?.to_str()?;

    // What's made for showing at a small size has less detail, but these aren't small.
    let size = dir
        .split('/')
        .find_map(|part| part.split('x').next()?.parse::<u32>().ok());

    match extension {
        "svg" => Some(1000),
        "png" => match size {
            Some(size) if size >= ICON_SIZE => Some(2000 - size.min(1000)),
            Some(size) => Some(size),
            None => Some(ICON_SIZE / 2),
        },
        _ => None,
    }
}

/// Adds the icons below `dir` to `index`, keeping the best file of each name.
fn index_icons(dir: &Path, index: &mut HashMap<String, (u32, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            index_icons(&path, index);
            continue;
        }

        let (Some(score), Some(name)) = (
            icon_score(&path),
            path.file_stem().and_then(|name| name.to_str()),
        ) else {
            continue;
        };

        match index.get_mut(name) {
            Some((best, _)) if *best >= score => (),
            Some(found) => *found = (score, path),
            None => {
                index.insert(name.to_owned(), (score, path));
            }
        }
    }
}

/// Icons by name, from the themes in order of preference.
fn index_all_icons(theme: &str) -> HashMap<String, PathBuf> {
    let mut index = HashMap::new();
    let dirs = data_dirs();

    for name in [theme].into_iter().chain(FALLBACK_THEMES) {
        let mut this_theme = HashMap::new();
        for dir in &dirs {
            index_icons(&dir.join("icons").join(name), &mut this_theme);
        }
        for (name, (_, path)) in this_theme {
            index.entry(name).or_insert(path);
        }
    }

    let mut pixmaps = HashMap::new();
    index_icons(Path::new("/usr/share/pixmaps"), &mut pixmaps);
    for (name, (_, path)) in pixmaps {
        index.entry(name).or_insert(path);
    }

    index
}

fn render_svg(path: &Path) -> Option<RgbaImage> {
    let tree = usvg::Tree::from_data(&fs::read(path).ok()?, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(ICON_SIZE, ICON_SIZE)?;

    let size = tree.size();
    let scale = ICON_SIZE as f32 / size.width().max(size.height());
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    // Slint wants straight alpha, and this is premultiplied.
    let pixels = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let pixel = pixel.demultiply();
            [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
        })
        .collect();
    RgbaImage::from_raw(ICON_SIZE, ICON_SIZE, pixels)
}

fn load_icon(path: &Path) -> Option<RgbaImage> {
    if path.extension().is_some_and(|extension| extension == "svg") {
        return render_svg(path);
    }

    let image = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    Some(imageops::thumbnail(
        &image.into_rgba8(),
        ICON_SIZE,
        ICON_SIZE,
    ))
}

/// Finds the apps on another thread, and sends them twice:
/// right away, and again with their icons, which takes a while.
pub fn scan(desktop: String, theme: String, send: impl Fn(Vec<App>) + Send + 'static) {
    let spawned = thread::Builder::new()
        .name("shell-apps".into())
        .spawn(move || {
            let mut apps = read_apps(&desktop);
            send(apps.clone());

            let index = index_all_icons(&theme);
            for app in &mut apps {
                let path = if app.icon_name.starts_with('/') {
                    Some(PathBuf::from(&app.icon_name))
                } else {
                    index.get(&app.icon_name).cloned()
                };
                app.icon = path.and_then(|path| load_icon(&path)).map(Arc::new);
            }
            send(apps);
        });

    if let Err(err) = spawned {
        warn!("Failed to start looking for apps: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_codes() {
        assert_eq!(strip_field_codes("firefox %u"), "firefox");
        assert_eq!(strip_field_codes("app --a %F --b"), "app --a --b");
        assert_eq!(strip_field_codes("echo 100%%"), "echo 100%");
    }

    fn parse(contents: &str) -> Option<App> {
        parse_desktop_entry("test", contents, "Pinnacle")
    }

    #[test]
    fn entries() {
        let app = parse(concat!(
            "[Desktop Entry]\nType=Application\nName=Files\nName[de]=Dateien\n",
            "Comment=Browse\nExec=nautilus %U\nIcon=org.gnome.Nautilus\n",
            "Keywords=folder;manager;\nTerminal=false\n",
            "[Desktop Action new]\nName=New Window\nExec=other\n",
        ))
        .unwrap();

        assert_eq!(app.name, "Files");
        assert_eq!(app.command, "nautilus");
        assert_eq!(app.keywords, ["folder", "manager"]);
        assert!(!app.terminal);
    }

    #[test]
    fn hidden_entries() {
        let entry =
            |extra: &str| format!("[Desktop Entry]\nType=Application\nName=A\nExec=a\n{extra}\n");

        assert!(parse(&entry("")).is_some());
        assert!(parse(&entry("NoDisplay=true")).is_none());
        assert!(parse(&entry("Hidden=true")).is_none());
        assert!(parse(&entry("OnlyShowIn=KDE;")).is_none());
        assert!(parse(&entry("OnlyShowIn=Pinnacle;")).is_some());
        assert!(parse(&entry("NotShowIn=Pinnacle;")).is_none());
        assert!(parse("[Desktop Entry]\nType=Link\nName=A\nExec=a").is_none());
    }

    #[test]
    fn icon_sizes() {
        let score = |path: &str| icon_score(Path::new(path));

        assert!(score("/i/128x128/apps/a.png") > score("/i/48x48/apps/a.png"));
        assert!(score("/i/128x128/apps/a.png") > score("/i/512x512/apps/a.png"));
        assert!(score("/i/128x128/apps/a.png") > score("/i/scalable/apps/a.svg"));
        assert!(score("/i/scalable/apps/a.svg") > score("/i/48x48/apps/a.png"));
        assert_eq!(score("/i/a.txt"), None);
    }
}
