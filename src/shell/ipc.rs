// SPDX-License-Identifier: GPL-3.0-or-later

//! The socket the settings app talks to the shell through.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::mpsc,
    thread,
};

use aether_common::Request;
use tracing::warn;

/// Listens on `path` on threads of its own.
///
/// `on_request` hands a request to the shell, along with where the answer goes.
/// `on_gone` is called when an app has disconnected.
pub fn serve(
    path: &Path,
    on_request: impl Fn(Request, mpsc::Sender<String>) -> bool + Clone + Send + 'static,
    on_gone: impl Fn() + Clone + Send + 'static,
) -> std::io::Result<()> {
    // It's left behind by a compositor that went away, as nothing cleans up after a crash.
    match fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
        _ => (),
    }
    let listener = UnixListener::bind(path)?;

    thread::Builder::new()
        .name("shell-ipc".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let on_request = on_request.clone();
                let on_gone = on_gone.clone();
                let spawned = thread::Builder::new()
                    .name("shell-ipc-client".into())
                    .spawn(move || {
                        serve_client(stream, on_request);
                        on_gone();
                    });
                if let Err(err) = spawned {
                    warn!("Failed to start a thread for a settings app: {err}");
                }
            }
        })?;
    Ok(())
}

fn serve_client(stream: UnixStream, on_request: impl Fn(Request, mpsc::Sender<String>) -> bool) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let mut writer = writer;

    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            return;
        };
        let request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                warn!("Got a request that makes no sense: {err}");
                return;
            }
        };

        let (reply, answer) = mpsc::channel();
        if !on_request(request, reply) {
            return;
        }
        let Ok(mut answer) = answer.recv() else {
            return;
        };
        answer.push('\n');
        if writer.write_all(answer.as_bytes()).is_err() {
            return;
        }
    }
}
