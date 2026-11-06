// SPDX-License-Identifier: GPL-3.0-or-later

//! What global search needs: matching text, a calculator and finding files.

use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
};

use super::apps::App;

/// The most files that are looked at.
const MAX_FILES: usize = 30_000;

/// How deep into folders to look.
const MAX_FILE_DEPTH: usize = 5;

/// Folders in the way of finding things.
const SKIPPED_FOLDERS: [&str; 4] = ["node_modules", "target", "__pycache__", "venv"];

/// How well `text` matches `query`, which is expected in lowercase. Higher is better.
pub fn match_score(text: &str, query: &str) -> Option<u32> {
    let text = text.to_lowercase();

    if text == query {
        Some(400)
    } else if text.starts_with(query) {
        Some(300)
    } else if text
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(query))
    {
        Some(200)
    } else {
        text.contains(query).then_some(100)
    }
}

/// How well an app matches `query`, for which its name counts the most.
pub fn app_score(app: &App, query: &str) -> Option<u32> {
    let keywords = app
        .keywords
        .iter()
        .filter_map(|keyword| match_score(keyword, query))
        .max()
        .map(|score| score / 2);
    let comment = match_score(&app.comment, query).map(|score| score / 4);

    match_score(&app.name, query).or(keywords).or(comment)
}

/// Finds files and folders in `home`, leaving out hidden ones. This goes one level at a
/// time, so that hitting the limit leaves out what's buried the deepest.
pub fn index_files(home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut folders = VecDeque::from([(home.to_owned(), 0)]);

    while let Some((folder, depth)) = folders.pop_front() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };

        for entry in entries.flatten() {
            if files.len() >= MAX_FILES {
                return files;
            }

            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIPPED_FOLDERS.contains(&name.as_ref()) {
                continue;
            }

            let path = entry.path();
            if depth < MAX_FILE_DEPTH && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                folders.push_back((path.clone(), depth + 1));
            }
            files.push(path);
        }
    }

    files
}

/// The address of a web search for `query`.
pub fn web_search_url(query: &str) -> String {
    let mut url = "https://www.google.com/search?q=".to_owned();

    for byte in query.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                url.push(byte as char);
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }

    url
}

/// Where a selection goes after moving it by `columns` and `rows`, among `apps` apps in a
/// grid `width` wide that are followed by a list, making for `total` items altogether.
pub fn next_selection(
    current: usize,
    columns: i32,
    rows: i32,
    apps: usize,
    total: usize,
    width: usize,
) -> usize {
    let (current_at, apps_at, width_at) = (current as i32, apps as i32, width as i32);
    let last = total as i32 - 1;

    let next = if current >= apps {
        // Going up from the list ends up on the last app.
        current_at + columns + rows
    } else {
        let next = current_at + columns + rows * width_at;
        // The last row of apps may not reach all the way across, and the list is next.
        if rows > 0 && next >= apps_at {
            if total > apps {
                apps_at
            } else if (apps_at - 1) / width_at > current_at / width_at {
                apps_at - 1
            } else {
                current_at
            }
        } else if rows < 0 && next < 0 {
            current_at
        } else {
            next
        }
    };

    next.clamp(0, last) as usize
}

/// A topic in the settings app.
pub struct Topic {
    /// Its place in the settings app's list.
    pub page: usize,
    pub title: &'static str,
    pub subtitle: &'static str,
    keywords: &'static [&'static str],
}

pub const TOPICS: [Topic; 8] = [
    Topic {
        page: 0,
        title: "Network & internet",
        subtitle: "Wi-Fi, saved networks",
        keywords: &["wifi", "wireless", "internet", "password", "connection"],
    },
    Topic {
        page: 1,
        title: "Connected devices",
        subtitle: "Bluetooth, pairing",
        keywords: &[
            "bluetooth",
            "headphones",
            "earbuds",
            "mouse",
            "keyboard",
            "pair",
        ],
    },
    Topic {
        page: 2,
        title: "Display",
        subtitle: "Brightness, Dark theme, scale",
        keywords: &[
            "brightness",
            "dark",
            "light",
            "night",
            "scale",
            "monitor",
            "screen",
        ],
    },
    Topic {
        page: 3,
        title: "Wallpaper & style",
        subtitle: "Wallpaper, colors",
        keywords: &["background", "theme", "colors", "picture"],
    },
    Topic {
        page: 4,
        title: "Sound",
        subtitle: "Volume, output, microphone",
        keywords: &["audio", "volume", "speaker", "microphone", "mute"],
    },
    Topic {
        page: 5,
        title: "Notifications",
        subtitle: "Do Not Disturb, clear all",
        keywords: &["alerts", "dnd", "disturb"],
    },
    Topic {
        page: 6,
        title: "Battery",
        subtitle: "Charge, Battery Saver",
        keywords: &["power", "saver", "charging", "stay", "awake", "idle"],
    },
    Topic {
        page: 7,
        title: "About",
        subtitle: "Device, software, hardware",
        keywords: &[
            "system",
            "version",
            "kernel",
            "memory",
            "processor",
            "hostname",
        ],
    },
];

/// The settings topics for `query`, which is expected in lowercase, best first.
/// All of them match nothing being asked.
pub fn matching_topics(query: &str) -> Vec<&'static Topic> {
    let mut topics = TOPICS
        .iter()
        .filter_map(|topic| {
            let title = match_score(topic.title, query);
            let subtitle = match_score(topic.subtitle, query).map(|score| score / 2);
            let keyword = topic
                .keywords
                .iter()
                .filter_map(|keyword| match_score(keyword, query))
                .max()
                .map(|score| score / 2);
            Some((title.or(subtitle).or(keyword)?, topic))
        })
        .collect::<Vec<_>>();

    topics.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    topics.into_iter().map(|(_, topic)| topic).collect()
}

/// A recursive descent parser for arithmetic.
struct Calculator<'a> {
    rest: &'a str,
}

impl Calculator<'_> {
    fn skip_spaces(&mut self) {
        self.rest = self.rest.trim_start();
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip_spaces();
        match self.rest.strip_prefix(c) {
            Some(rest) => {
                self.rest = rest;
                true
            }
            None => false,
        }
    }

    fn sum(&mut self) -> Option<f64> {
        let mut value = self.product()?;
        loop {
            if self.eat('+') {
                value += self.product()?;
            } else if self.eat('-') {
                value -= self.product()?;
            } else {
                return Some(value);
            }
        }
    }

    fn product(&mut self) -> Option<f64> {
        let mut value = self.power()?;
        loop {
            if self.eat('*') {
                value *= self.power()?;
            } else if self.eat('/') {
                value /= self.power()?;
            } else if self.eat('%') {
                value %= self.power()?;
            } else {
                return Some(value);
            }
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        // This goes from right to left.
        if self.eat('^') {
            Some(base.powf(self.power()?))
        } else {
            Some(base)
        }
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat('-') {
            return Some(-self.unary()?);
        }
        self.atom()
    }

    fn atom(&mut self) -> Option<f64> {
        if self.eat('(') {
            let value = self.sum()?;
            return self.eat(')').then_some(value);
        }

        self.skip_spaces();
        let end = self
            .rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(self.rest.len());
        let (number, rest) = self.rest.split_at(end);
        self.rest = rest;
        number.parse().ok()
    }
}

/// Works out `expression`, if it's arithmetic with something to work out.
pub fn calculate(expression: &str) -> Option<String> {
    let is_sum = expression.contains(|c: char| "+-*/%^(".contains(c));
    if !is_sum {
        return None;
    }

    let mut calculator = Calculator { rest: expression };
    let value = calculator.sum()?;
    calculator.skip_spaces();

    (calculator.rest.is_empty() && value.is_finite()).then(|| {
        let rounded = (value * 1e10).round() / 1e10;
        format!("{rounded}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching() {
        assert_eq!(match_score("Firefox", "firefox"), Some(400));
        assert!(match_score("Firefox", "fire") > match_score("Web Firefox", "fire"));
        assert!(match_score("Web Firefox", "fire") > match_score("Campfire", "fire"));
        assert_eq!(match_score("Firefox", "chrome"), None);
    }

    #[test]
    fn apps_are_found_by_more_than_their_name() {
        let app = App {
            id: "a".into(),
            name: "Files".into(),
            comment: "Access and organize files".into(),
            keywords: vec!["folder".into()],
            command: "a".into(),
            terminal: false,
            icon_name: String::new(),
            icon: None,
        };

        assert!(app_score(&app, "files") > app_score(&app, "folder"));
        assert!(app_score(&app, "folder") > app_score(&app, "organize"));
        assert_eq!(app_score(&app, "music"), None);
    }

    #[test]
    fn topics() {
        assert_eq!(matching_topics("").len(), TOPICS.len());
        assert_eq!(matching_topics("bluetooth")[0].title, "Connected devices");
        assert_eq!(matching_topics("wall")[0].title, "Wallpaper & style");
        assert_eq!(matching_topics("volume")[0].title, "Sound");
        assert!(matching_topics("qqq").is_empty());
    }

    #[test]
    fn moving_through_the_grid() {
        // 7 apps in rows of 3, then 2 results.
        let go = |current, columns, rows| next_selection(current, columns, rows, 7, 9, 3);

        assert_eq!(go(0, 1, 0), 1);
        assert_eq!(go(0, 0, 1), 3);
        assert_eq!(go(4, 0, -1), 1);
        assert_eq!(go(1, 0, -1), 1);
        // Down from the last rows ends up on the first result.
        assert_eq!(go(5, 0, 1), 7);
        assert_eq!(go(6, 0, 1), 7);
        assert_eq!(go(7, 0, 1), 8);
        assert_eq!(go(8, 0, 1), 8);
        // Up from the first result ends up on the last app.
        assert_eq!(go(7, 0, -1), 6);
    }

    #[test]
    fn moving_without_results() {
        let go = |current, rows| next_selection(current, 0, rows, 7, 7, 3);

        // There's something below this, even if not right below it.
        assert_eq!(go(5, 1), 6);
        assert_eq!(go(6, 1), 6);
    }

    #[test]
    fn moving_through_only_results() {
        assert_eq!(next_selection(0, 0, 1, 0, 3, 3), 1);
        assert_eq!(next_selection(2, 0, 1, 0, 3, 3), 2);
        assert_eq!(next_selection(0, 0, -1, 0, 3, 3), 0);
    }

    #[test]
    fn web_addresses() {
        assert_eq!(
            web_search_url("rust & wayland"),
            "https://www.google.com/search?q=rust%20%26%20wayland"
        );
    }

    #[test]
    fn calculating() {
        let result = |text| calculate(text);

        assert_eq!(result("2 + 3 * 4").as_deref(), Some("14"));
        assert_eq!(result("(2 + 3) * 4").as_deref(), Some("20"));
        assert_eq!(result("2 ^ 3 ^ 2").as_deref(), Some("512"));
        assert_eq!(result("-2 + 5").as_deref(), Some("3"));
        assert_eq!(result("10 / 4").as_deref(), Some("2.5"));
        assert_eq!(result("0.1 + 0.2").as_deref(), Some("0.3"));
        assert_eq!(result("10 % 3").as_deref(), Some("1"));
    }

    #[test]
    fn not_calculating() {
        assert_eq!(calculate("firefox"), None);
        assert_eq!(calculate("42"), None);
        assert_eq!(calculate("2 +"), None);
        assert_eq!(calculate("1 / 0"), None);
        assert_eq!(calculate("(1 + 2"), None);
        assert_eq!(calculate("rust-analyzer"), None);
    }
}
