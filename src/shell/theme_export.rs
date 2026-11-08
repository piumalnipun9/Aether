// SPDX-License-Identifier: GPL-3.0-or-later

//! Sharing the wallpaper's theme with other apps.
//!
//! `aether/colors.json` has every Material role for both the dark and light theme, for
//! anything that wants to read them, and `aether/colors.css` the same as GTK named colors.
//! Apps with a theme format of their own get a file in it: GTK, foot, kitty, Qt, yazi, fastfetch and an icon theme.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use material_colors::{color::Argb, hct::Hct, theme::ThemeBuilder};
use serde_json::{Map, Value, json};

use super::{
    folder_icons,
    wallpaper::{Palettes, Rgb},
};

/// What marks a `gtk.css` as ours to replace.
const MARKER: &str = "/* Written by Aether";
/// The same for files with `#` comments.
const HASH_MARKER: &str = "# Written by Aether";

const HEADER: &str =
    "/* Written by Aether from the wallpaper's colors. Changes here get replaced. */\n";

/// How see-through terminals are when what's behind them is blurred.
const TERMINAL_ALPHA: f64 = 0.82;

/// Yazi's own colors for what's not a folder, which has to be repeated to change those.
const YAZI_FILE_RULES: &str = r#"    { mime = "**/image/*", fg = "yellow" },
    { mime = "**/{audio,video}/*", fg = "magenta" },
    { mime = "**/application/{zip,rar,7z*,tar,gzip,xz,zstd,bzip*,lzma,compress,archive,cpio,arj,xar,ms-cab*}", fg = "red" },
    { mime = "**/application/{pdf,doc,rtf}", fg = "cyan" },
    { mime = "vfs/{absent,stale}", fg = "darkgray" },
    { url = "*", is = "orphan", bg = "red" },
    { url = "*", is = "exec", fg = "green" },
    { url = "*", is = "dummy", bg = "red" },
    { url = "*/", is = "dummy", bg = "red" },
"#;

/// The Aether logo in characters, like the logos fastfetch has for distributions.
const LOGO: &str = include_str!("../../assets/aether-logo.txt");

/// What fastfetch shows without a config, which a config has to list to keep.
const FASTFETCH_MODULES: &str = r#"["title", "separator", "os", "host", "kernel", "uptime", "packages", "shell", "display", "de", "wm", "wmtheme", "theme", "icons", "font", "cursor", "terminal", "terminalfont", "cpu", "gpu", "memory", "swap", "disk", "localip", "battery", "poweradapter", "locale", "break", "colors"]"#;

/// What marks a fastfetch config as ours.
const SLASH_MARKER: &str = "// Written by Aether";

/// The hues of the terminal colors, from red to cyan in the order of the ANSI ones.
const ANSI_HUES: [f64; 6] = [25.0, 145.0, 90.0, 265.0, 320.0, 200.0];
/// How far terminal colors move towards the theme's own.
const ANSI_HARMONY: f64 = 0.15;
const ANSI_CHROMA: f64 = 55.0;

type Roles = Vec<(&'static str, Rgb)>;

fn roles(source: Argb, dark: bool) -> Roles {
    let schemes = ThemeBuilder::with_source(source).build().schemes;
    let s = if dark { schemes.dark } else { schemes.light };
    let rgb = |color: Argb| [color.red, color.green, color.blue];

    macro_rules! roles {
        ($($name:ident),* $(,)?) => {
            vec![$((stringify!($name), rgb(s.$name))),*]
        };
    }

    roles![
        primary,
        on_primary,
        primary_container,
        on_primary_container,
        inverse_primary,
        secondary,
        on_secondary,
        secondary_container,
        on_secondary_container,
        tertiary,
        on_tertiary,
        tertiary_container,
        on_tertiary_container,
        error,
        on_error,
        error_container,
        on_error_container,
        surface_dim,
        surface,
        surface_bright,
        surface_container_lowest,
        surface_container_low,
        surface_container,
        surface_container_high,
        surface_container_highest,
        on_surface,
        on_surface_variant,
        outline,
        outline_variant,
        inverse_surface,
        inverse_on_surface,
        surface_variant,
        background,
        on_background,
    ]
}

fn role(roles: &Roles, name: &str) -> Rgb {
    roles
        .iter()
        .find(|(role, _)| *role == name)
        .map_or([0, 0, 0], |(_, color)| *color)
}

fn hex([red, green, blue]: Rgb) -> String {
    format!("#{red:02x}{green:02x}{blue:02x}")
}

/// `hue` moved a bit towards `towards`, the short way around.
fn harmonize(hue: f64, towards: f64) -> f64 {
    let difference = (towards - hue + 540.0).rem_euclid(360.0) - 180.0;
    (hue + difference * ANSI_HARMONY).rem_euclid(360.0)
}

/// The 16 terminal colors.
fn ansi(source: Argb, dark: bool, roles: &Roles) -> [Rgb; 16] {
    let source_hue = Hct::new(source).get_hue();
    let color = |hue: f64, tone: f64| {
        let color: Argb = Hct::from(harmonize(hue, source_hue), ANSI_CHROMA, tone).into();
        [color.red, color.green, color.blue]
    };
    let (normal, bright) = if dark { (72.0, 84.0) } else { (40.0, 32.0) };

    let mut colors = [[0; 3]; 16];
    colors[0] = role(roles, "surface_container_highest");
    colors[7] = role(roles, "on_surface_variant");
    colors[8] = role(roles, "outline");
    colors[15] = role(roles, "on_surface");
    for (index, hue) in ANSI_HUES.into_iter().enumerate() {
        colors[index + 1] = color(hue, normal);
        colors[index + 9] = color(hue, bright);
    }
    colors
}

fn colors_json(palettes: &Palettes, dark: bool) -> String {
    let map = |roles: Roles| {
        roles
            .into_iter()
            .map(|(name, color)| (name.to_owned(), Value::from(hex(color))))
            .collect::<Map<_, _>>()
    };
    let source = palettes.source;
    let (dark_roles, light_roles) = (roles(source, true), roles(source, false));
    let (dark_ansi, light_ansi) = (
        ansi(source, true, &dark_roles),
        ansi(source, false, &light_roles),
    );
    // The terminal colors, in the format Dank Material Shell shares them in.
    let terminal = dark_ansi
        .into_iter()
        .zip(light_ansi)
        .enumerate()
        .map(|(index, (dark, light))| {
            (
                format!("color{index}"),
                json!({ "dark": hex(dark), "light": hex(light) }),
            )
        })
        .collect::<Map<_, _>>();

    let json = json!({
        "mode": if dark { "dark" } else { "light" },
        "source": hex([source.red, source.green, source.blue]),
        "colors": {
            "dark": map(dark_roles),
            "light": map(light_roles),
        },
        "dank16": terminal,
    });
    serde_json::to_string_pretty(&json).unwrap_or_default() + "\n"
}

fn colors_css(roles: &Roles) -> String {
    let mut css = String::from(HEADER);
    for (name, color) in roles {
        css += &format!("@define-color {name} {};\n", hex(*color));
    }
    css
}

/// The colors libadwaita and GTK 4's own theme go by.
fn gtk4_css(roles: &Roles) -> String {
    let color = |name| hex(role(roles, name));
    let defines = [
        ("accent_color", "primary"),
        ("accent_bg_color", "primary"),
        ("accent_fg_color", "on_primary"),
        ("destructive_color", "error"),
        ("destructive_bg_color", "error"),
        ("destructive_fg_color", "on_error"),
        ("error_color", "error"),
        ("error_bg_color", "error"),
        ("error_fg_color", "on_error"),
        ("window_bg_color", "surface"),
        ("window_fg_color", "on_surface"),
        ("view_bg_color", "surface_container_low"),
        ("view_fg_color", "on_surface"),
        ("headerbar_bg_color", "surface_container"),
        ("headerbar_fg_color", "on_surface"),
        ("headerbar_border_color", "outline_variant"),
        ("headerbar_backdrop_color", "surface"),
        ("card_bg_color", "surface_container"),
        ("card_fg_color", "on_surface"),
        ("popover_bg_color", "surface_container_high"),
        ("popover_fg_color", "on_surface"),
        ("dialog_bg_color", "surface_container_high"),
        ("dialog_fg_color", "on_surface"),
    ];

    let mut css = String::from(HEADER);
    for (name, role) in defines {
        css += &format!("@define-color {name} {};\n", color(role));
    }
    css += &format!(
        ".navigation-sidebar, .sidebar {{\n    background-color: {};\n    color: {};\n}}\n",
        color("surface_container"),
        color("on_surface"),
    );
    css
}

/// GTK 3 themes are mostly fixed colors, so this has to go after the common widgets.
fn gtk3_css(roles: &Roles) -> String {
    let color = |name| hex(role(roles, name));
    let defines = [
        ("theme_bg_color", "surface"),
        ("theme_fg_color", "on_surface"),
        ("theme_base_color", "surface_container_low"),
        ("theme_text_color", "on_surface"),
        ("theme_selected_bg_color", "primary"),
        ("theme_selected_fg_color", "on_primary"),
        ("theme_unfocused_bg_color", "surface"),
        ("theme_unfocused_fg_color", "on_surface_variant"),
        ("borders", "outline_variant"),
    ];

    let mut css = String::from(HEADER);
    for (name, role) in defines {
        css += &format!("@define-color {name} {};\n", color(role));
    }
    css += &format!(
        "\
window, .background {{
    background-color: @theme_bg_color;
    color: @theme_fg_color;
}}
headerbar, .titlebar, toolbar, .toolbar, menubar {{
    background-color: {header};
    background-image: none;
    color: @theme_fg_color;
    border-color: @borders;
}}
.sidebar, .sidebar .view, .sidebar list {{
    background-color: {header};
    color: @theme_fg_color;
}}
.view, iconview, treeview.view, textview text, list {{
    background-color: @theme_base_color;
    color: @theme_text_color;
}}
.view:selected, iconview:selected, treeview.view:selected, list row:selected, selection {{
    background-color: @theme_selected_bg_color;
    color: @theme_selected_fg_color;
}}
",
        header = color("surface_container"),
    );
    css
}

fn foot_ini(source: Argb, dark: bool, roles: &Roles, blur: bool) -> String {
    let plain = |color: Rgb| hex(color)[1..].to_owned();
    let ansi = ansi(source, dark, roles);

    let mut ini = String::from("# Written by Aether from the wallpaper's colors.\n[colors]\n");
    ini += &format!("alpha={}\n", if blur { TERMINAL_ALPHA } else { 1.0 });
    ini += &format!("background={}\n", plain(role(roles, "surface")));
    ini += &format!("foreground={}\n", plain(role(roles, "on_surface")));
    ini += &format!("selection-background={}\n", plain(role(roles, "primary")));
    ini += &format!(
        "selection-foreground={}\n",
        plain(role(roles, "on_primary"))
    );
    for (index, color) in ansi.into_iter().enumerate() {
        let (kind, number) = if index < 8 {
            ("regular", index)
        } else {
            ("bright", index - 8)
        };
        ini += &format!("{kind}{number}={}\n", plain(color));
    }
    ini += &format!(
        "[cursor]\ncolor={} {}\n",
        plain(role(roles, "on_primary")),
        plain(role(roles, "primary"))
    );
    ini
}

fn kitty_conf(source: Argb, dark: bool, roles: &Roles, blur: bool) -> String {
    let color = |name| hex(role(roles, name));
    let mut conf = String::from("# Written by Aether from the wallpaper's colors.\n");
    conf += &format!(
        "background_opacity {}\n",
        if blur { TERMINAL_ALPHA } else { 1.0 }
    );
    for (key, name) in [
        ("background", "surface"),
        ("foreground", "on_surface"),
        ("selection_background", "primary"),
        ("selection_foreground", "on_primary"),
        ("cursor", "primary"),
        ("cursor_text_color", "on_primary"),
        ("active_tab_background", "primary"),
        ("active_tab_foreground", "on_primary"),
        ("inactive_tab_background", "surface_container"),
        ("inactive_tab_foreground", "on_surface_variant"),
    ] {
        conf += &format!("{key} {}\n", color(name));
    }
    for (index, value) in ansi(source, dark, roles).into_iter().enumerate() {
        conf += &format!("color{index} {}\n", hex(value));
    }
    conf
}

/// A color scheme for qt5ct and qt6ct.
fn qt_scheme(roles: &Roles) -> String {
    let color = |name| hex(role(roles, name));
    // In the order of QPalette's color roles.
    let active = [
        "on_surface",
        "surface_container",
        "surface_container_highest",
        "surface_container_high",
        "surface_dim",
        "surface_container_low",
        "on_surface",
        "on_surface",
        "on_surface",
        "surface_container_low",
        "surface",
        "surface_container_lowest",
        "primary",
        "on_primary",
        "primary",
        "tertiary",
        "surface_container",
        "surface",
        "surface_container_high",
        "on_surface",
        "on_surface_variant",
    ]
    .map(color)
    .join(", ");
    let disabled = [("on_surface", "outline"), ("on_surface_variant", "outline")]
        .iter()
        .fold(active.clone(), |text, (from, to)| {
            text.replace(&color(from), &color(to))
        });

    format!(
        "[ColorScheme]\nactive_colors={active}\ndisabled_colors={disabled}\n\
         inactive_colors={active}\n"
    )
}

/// The colors of folders and the selection for yazi, which picks all the other colors from
/// the terminal's.
fn yazi_theme(accent: Rgb, roles: &Roles) -> String {
    let fg = hex(accent);
    let text = hex(role(roles, "on_surface"));
    let surface = hex(role(roles, "surface"));
    let shade = hex(role(roles, "surface_container_highest"));
    let selected = format!("{{ fg = \"{surface}\", bg = \"{text}\" }}");
    // The bottom bar: the mode, then what's next to it, in the colors of each mode.
    let mode = |name: &str, color: &str| {
        format!(
            "{name}_main = {{ fg = \"{surface}\", bg = \"{color}\", bold = true }}\n\
             {name}_alt = {{ fg = \"{color}\", bg = \"{shade}\" }}\n"
        )
    };
    let modes =
        mode("normal", &text) + &mode("select", &fg) + &mode("unset", &hex(role(roles, "error")));
    let dirs = folder_icons::YAZI_DIRS
        .iter()
        .map(|(name, icon)| {
            format!("    {{ name = \"{name}\", text = \"{icon}\", fg = \"{fg}\" }},\n")
        })
        .collect::<String>();
    let (open, closed) = (folder_icons::YAZI_OPEN_DIR, folder_icons::YAZI_DIR);

    format!(
        "{HASH_MARKER} from the wallpaper's colors. Changes here get replaced.\n\
         # Only folders and the selection are set, so yazi picks everything else from the\n\
         # terminal's colors.\n\
         [indicator]\n\
         parent = {selected}\n\
         current = {selected}\n\
         preview = {selected}\n\n\
         [mode]\n\
         {modes}\n\
         [filetype]\n\
         rules = [\n{YAZI_FILE_RULES}    {{ url = \"*/\", fg = \"{text}\" }},\n]\n\n\
         [icon]\n\
         prepend_dirs = [\n{dirs}]\n\
         prepend_conds = [\n    \
         {{ if = \"dir & hovered\", text = \"{open}\", fg = \"{fg}\" }},\n    \
         {{ if = \"dir\", text = \"{closed}\", fg = \"{fg}\" }},\n\
         ]\n"
    )
}

/// fastfetch's config, with the logo, the title and the keys in the wallpaper's color.
fn fastfetch_config(accent: Rgb, logo: &Path) -> String {
    let color = hex(accent);
    let logo = serde_json::to_string(&logo.to_string_lossy()).unwrap_or_default();
    format!(
        "{SLASH_MARKER} from the wallpaper's colors. Changes here get replaced.\n\
         {{\n  \
         \"$schema\": \"https://github.com/fastfetch-cli/fastfetch/raw/dev/doc/json_schema.json\",\n  \
         \"logo\": {{\n    \
         \"type\": \"file\",\n    \
         \"source\": {logo},\n    \
         \"color\": {{ \"1\": \"{color}\" }},\n    \
         \"padding\": {{ \"top\": 1, \"right\": 4 }}\n  \
         }},\n  \
         \"display\": {{\n    \
         \"color\": {{ \"keys\": \"{color}\", \"title\": \"{color}\" }}\n  \
         }},\n  \
         \"modules\": {FASTFETCH_MODULES}\n\
         }}\n"
    )
}

/// The description of the icon theme.
fn icon_index(dark: bool) -> String {
    let inherits = if dark {
        "Papirus-Dark,Papirus,hicolor"
    } else {
        "Papirus,Papirus-Dark,hicolor"
    };
    format!(
        "[Icon Theme]\n\
         Name=Aether\n\
         Comment=Folders in the wallpaper's colors, on top of Papirus\n\
         Inherits={inherits}\n\
         Directories=scalable/places\n\n\
         [scalable/places]\n\
         Size=64\n\
         MinSize=8\n\
         MaxSize=512\n\
         Context=Places\n\
         Type=Scalable\n"
    )
}

/// Writes the icon theme into `dir`.
fn write_icon_theme(dir: &Path, source: Argb, dark: bool) -> std::io::Result<()> {
    write(&dir.join("index.theme"), &icon_index(dark))?;
    for (name, symbol) in folder_icons::ICONS {
        write(
            &dir.join("scalable/places").join(format!("{name}.svg")),
            &folder_icons::folder_svg(source, symbol),
        )?;
    }
    // This is how apps notice that a theme has changed.
    fs::File::open(dir)?.set_modified(std::time::SystemTime::now())
}

/// What tells a terminal to use these colors, for terminals that are already open.
fn terminal_sequences(source: Argb, dark: bool, roles: &Roles) -> String {
    let color = |[red, green, blue]: Rgb| format!("rgb:{red:02x}/{green:02x}/{blue:02x}");
    let osc = |code: &str, value: String| format!("\x1b]{code};{value}\x1b\\");

    let mut sequences = ansi(source, dark, roles)
        .into_iter()
        .enumerate()
        .map(|(index, value)| osc(&format!("4;{index}"), color(value)))
        .collect::<String>();
    for (code, name) in [
        ("10", "on_surface"),
        ("11", "surface"),
        ("12", "primary"),
        ("17", "primary"),
        ("19", "on_primary"),
    ] {
        sequences += &osc(code, color(role(roles, name)));
    }
    sequences
}

/// The terminals of the open foot windows.
fn foot_terminals() -> Vec<PathBuf> {
    let Ok(processes) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    // A process's name and its parent, from its stat line.
    let stat = |pid: &str| -> Option<(String, String)> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (name, rest) = stat.split_once(" (")?.1.rsplit_once(") ")?;
        Some((name.to_owned(), rest.split(' ').nth(1)?.to_owned()))
    };

    let pids = processes
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        .filter_map(|pid| Some((stat(&pid)?, pid)))
        .collect::<Vec<_>>();
    let feet = pids
        .iter()
        .filter(|((name, _), _)| name == "foot")
        .map(|(_, pid)| pid.as_str())
        .collect::<Vec<_>>();

    let mut terminals = pids
        .iter()
        .filter(|((_, parent), _)| feet.contains(&parent.as_str()))
        .filter_map(|(_, pid)| fs::read_link(format!("/proc/{pid}/fd/0")).ok())
        .filter(|path| path.starts_with("/dev/pts"))
        .collect::<Vec<_>>();
    terminals.sort();
    terminals.dedup();
    terminals
}

/// Gives the open foot windows the new colors. Foot only reads its config when a window
/// opens, but it takes colors from what's written to it.
fn recolor_terminals(source: Argb, dark: bool, roles: &Roles) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};

    let sequences = terminal_sequences(source, dark, roles);
    for terminal in foot_terminals() {
        // Not blocking, as a terminal that's stuck mustn't hold this up.
        let written = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(&terminal)
            .and_then(|mut file| file.write_all(sequences.as_bytes()));
        if let Err(err) = written {
            tracing::debug!("Failed to recolor {}: {err}", terminal.display());
        }
    }
}

/// `contents` of a GTK settings file with the dark theme set to `dark`.
fn with_gtk_dark(contents: &str, dark: bool) -> String {
    const KEY: &str = "gtk-application-prefer-dark-theme";
    let line = format!("{KEY}={}", u8::from(dark));

    let mut lines = contents.lines().collect::<Vec<_>>();
    if let Some(index) = lines.iter().position(|text| text.starts_with(KEY)) {
        lines[index] = &line;
    } else {
        if lines.is_empty() {
            lines.push("[Settings]");
        }
        lines.push(&line);
    }
    lines.join("\n") + "\n"
}

/// Writes `path` if that changes anything, as there may be apps watching it.
fn write(path: &Path, contents: &str) -> std::io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

/// Writes a file that starts with `marker` when it's ours. One that's not ours gets kept
/// next to it, once.
fn write_owned(path: &Path, marker: &str, contents: &str) -> std::io::Result<()> {
    if let Ok(current) = fs::read_to_string(path)
        && !current.starts_with(marker)
    {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".before-aether");
        let backup = PathBuf::from(backup);
        if !backup.exists() {
            fs::write(backup, current)?;
        }
    }
    write(path, contents)
}

fn export_to(
    config: &Path,
    data: &Path,
    palettes: &Palettes,
    dark: bool,
    blur: bool,
) -> std::io::Result<()> {
    let source = palettes.source;
    let roles = roles(source, dark);

    write(
        &config.join("aether/colors.json"),
        &colors_json(palettes, dark),
    )?;
    write(&config.join("aether/colors.css"), &colors_css(&roles))?;

    write_owned(&config.join("gtk-4.0/gtk.css"), MARKER, &gtk4_css(&roles))?;
    write_owned(&config.join("gtk-3.0/gtk.css"), MARKER, &gtk3_css(&roles))?;
    for dir in ["gtk-3.0", "gtk-4.0"] {
        let path = config.join(dir).join("settings.ini");
        let contents = fs::read_to_string(&path).unwrap_or_default();
        write(&path, &with_gtk_dark(&contents, dark))?;
    }

    write(
        &config.join("foot/aether-colors.ini"),
        &foot_ini(source, dark, &roles, blur),
    )?;
    write(
        &config.join("kitty/aether-colors.conf"),
        &kitty_conf(source, dark, &roles, blur),
    )?;
    write_owned(
        &config.join("yazi/theme.toml"),
        HASH_MARKER,
        &yazi_theme(palettes.get(dark).accent, &roles),
    )?;
    write_icon_theme(&data.join("icons/Aether"), source, dark)?;
    let logo = config.join("fastfetch/aether-logo.txt");
    write(&logo, &format!("$1{LOGO}"))?;
    write_owned(
        &config.join("fastfetch/config.jsonc"),
        SLASH_MARKER,
        &fastfetch_config(palettes.get(dark).accent, &logo),
    )?;
    for dir in ["qt5ct", "qt6ct"] {
        write(
            &config.join(dir).join("colors/Aether.conf"),
            &qt_scheme(&roles),
        )?;
    }

    Ok(())
}

fn config_home() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config")))
}

fn data_home() -> Option<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".local/share")))
}

/// Shares the theme. This touches a lot of files, so call it on another thread.
pub fn export(palettes: &Palettes, dark: bool, blur: bool) {
    let (Some(config), Some(data)) = (config_home(), data_home()) else {
        return;
    };
    if let Err(err) = export_to(&config, &data, palettes, dark, blur) {
        tracing::warn!("Failed to share the theme: {err}");
    }
    recolor_terminals(palettes.source, dark, &roles(palettes.source, dark));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("aether-export-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn it_all_gets_written() {
        let dir = temp_dir("all");
        export_to(&dir, &dir, &Palettes::fallback(), true, true).unwrap();

        let yazi = fs::read_to_string(dir.join("yazi/theme.toml")).unwrap();
        let yazi: toml::Value = toml::from_str(&yazi).unwrap();
        assert_eq!(yazi["icon"]["prepend_conds"][1]["if"].as_str(), Some("dir"));
        let rules = yazi["filetype"]["rules"].as_array().unwrap();
        assert_eq!(rules.last().unwrap()["url"].as_str(), Some("*/"));
        assert!(yazi["indicator"]["current"]["bg"].is_str());
        assert_eq!(yazi["mode"]["normal_main"]["bold"].as_bool(), Some(true));

        let index = fs::read_to_string(dir.join("icons/Aether/index.theme")).unwrap();
        assert!(index.contains("Inherits=Papirus-Dark"));
        assert!(
            dir.join("icons/Aether/scalable/places/folder-download.svg")
                .exists()
        );

        let fastfetch = fs::read_to_string(dir.join("fastfetch/config.jsonc")).unwrap();
        let json = fastfetch.split_once('\n').unwrap().1;
        let json: Value = serde_json::from_str(json).unwrap();
        assert!(
            json["logo"]["color"]["1"]
                .as_str()
                .unwrap()
                .starts_with('#')
        );
        assert_eq!(json["modules"][2].as_str(), Some("os"));
        let logo = fs::read_to_string(dir.join("fastfetch/aether-logo.txt")).unwrap();
        assert!(logo.starts_with("$1ooo"));

        let json: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("aether/colors.json")).unwrap())
                .unwrap();
        assert_eq!(json["mode"], "dark");
        assert!(json["colors"]["light"]["surface_container_high"].is_string());
        assert!(json["dank16"]["color15"]["dark"].is_string());

        let foot = fs::read_to_string(dir.join("foot/aether-colors.ini")).unwrap();
        assert!(foot.contains("alpha=0.82"));
        assert!(foot.contains("regular0=") && foot.contains("bright7="));

        let settings = fs::read_to_string(dir.join("gtk-4.0/settings.ini")).unwrap();
        assert_eq!(
            settings,
            "[Settings]\ngtk-application-prefer-dark-theme=1\n"
        );

        // Qt wants exactly this many colors.
        let qt = fs::read_to_string(dir.join("qt5ct/colors/Aether.conf")).unwrap();
        let active = qt.lines().nth(1).unwrap();
        assert_eq!(active.split(", ").count(), 21);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn terminals_are_solid_without_blur() {
        let dir = temp_dir("solid");
        export_to(&dir, &dir, &Palettes::fallback(), false, false).unwrap();
        let foot = fs::read_to_string(dir.join("foot/aether-colors.ini")).unwrap();
        assert!(foot.contains("alpha=1\n"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn someone_elses_gtk_css_is_kept_once() {
        let dir = temp_dir("css");
        let css = dir.join("gtk.css");

        fs::write(&css, "/* mine */\n").unwrap();
        write_owned(&css, MARKER, &format!("{HEADER}a\n")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("gtk.css.before-aether")).unwrap(),
            "/* mine */\n"
        );

        // Our own doesn't replace the backup.
        write_owned(&css, MARKER, &format!("{HEADER}b\n")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("gtk.css.before-aether")).unwrap(),
            "/* mine */\n"
        );
        assert_eq!(fs::read_to_string(&css).unwrap(), format!("{HEADER}b\n"));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_terminals_get_every_color() {
        let palettes = Palettes::fallback();
        let roles = roles(palettes.source, true);
        let sequences = terminal_sequences(palettes.source, true, &roles);

        assert_eq!(sequences.matches("\x1b]4;").count(), 16);
        for code in ["10", "11", "12", "17", "19"] {
            assert!(sequences.contains(&format!("\x1b]{code};rgb:")));
        }
        // Each one ends, so none of them swallows the next.
        assert_eq!(sequences.matches("\x1b\\").count(), 21);
    }

    #[test]
    fn the_dark_setting_changes_only_its_line() {
        assert_eq!(
            with_gtk_dark(
                "[Settings]\nother=1\ngtk-application-prefer-dark-theme=0\n",
                true
            ),
            "[Settings]\nother=1\ngtk-application-prefer-dark-theme=1\n"
        );
    }

    #[test]
    fn hues_go_the_short_way_around() {
        assert!((harmonize(350.0, 10.0) - 353.0).abs() < 0.001);
        assert!((harmonize(10.0, 350.0) - 7.0).abs() < 0.001);
    }
}
