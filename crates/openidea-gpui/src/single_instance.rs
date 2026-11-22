//! Single-instance guard.
//!
//! The Tauri shell used `tauri-plugin-single-instance`: a second launch forwarded
//! its arguments to the running app and exited. GPUI has no such plugin, so this
//! module implements the same contract with a Unix domain socket (a file whose
//! exact path is the claim, which works on macOS and Linux alike):
//!
//! * the first process creates the socket and listens;
//! * a later process connects, writes its arguments and returns `Handed`, so
//!   `main` exits without opening a second window;
//! * the owner accepts connections on a thread and forwards the argument list to
//!   the app, which opens the project/file in its existing window.
//!
//! A stale socket (a crashed process left the file behind) is detected by the
//! failed connect and replaced, so a crash does not lock the user out.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

/// How a launch was resolved.
pub enum Claim {
    /// This process owns the instance; the guard must live as long as the app.
    Owned(Guard),
    /// Another instance is running and received these arguments.
    Handed(Vec<String>),
}

/// The socket this process owns.
pub struct Guard {
    path: PathBuf,
    /// Arguments a later launch sent (a project or file to open).
    incoming: Receiver<Vec<String>>,
}

impl Guard {
    /// Takes the receiver so the app can open forwarded paths as they arrive.
    pub fn take_incoming(&mut self) -> Option<Receiver<Vec<String>>> {
        // `Receiver` is not `Clone`; hand the one out and keep polling nothing.
        Some(std::mem::replace(&mut self.incoming, std::sync::mpsc::channel().1))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // The socket file is ours; leave no stale path behind.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Claims the instance at `socket`, or hands `arguments` to the process that
/// already owns it.
pub fn acquire(socket: &Path) -> Result<Guard, Claim> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match UnixStream::connect(socket) {
        Ok(mut stream) => {
            // One argument per line; blank lines are not arguments.
            let _ = stream.write_all(
                arguments.join("\n").as_bytes(),
            );
            let _ = stream.flush();
            Err(Claim::Handed(arguments))
        }
        Err(_) => {
            // Nobody is listening: either a first launch or a stale socket. A
            // stale file would make `bind` fail, so remove it first.
            let _ = std::fs::remove_file(socket);
            match bind(socket) {
                Ok(listener) => {
                    let (incoming_tx, incoming) = std::sync::mpsc::channel();
                    std::thread::Builder::new()
                        .name("openidea-single-instance".into())
                        .spawn(move || listen(listener, incoming_tx))
                        .expect("spawn single-instance listener");
                    Err(Claim::Owned(Guard { path: socket.to_path_buf(), incoming }))
                }
                // A racing second launch won the bind after all: hand over.
                Err(_) => Err(Claim::Handed(arguments)),
            }
        }
    }
}

fn bind(socket: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    UnixListener::bind(socket)
}

fn listen(listener: UnixListener, incoming: Sender<Vec<String>>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        // A handover message is a small line-oriented argument list.
        let mut text = String::new();
        if std::io::Read::read_to_string(&mut stream, &mut text).is_ok()
            && incoming.send(split_arguments(&text)).is_err()
        {
            break;
        }
    }
}

/// Splits the handover payload into arguments (`\n` separated, no blank entries).
pub fn split_arguments(text: &str) -> Vec<String> {
    text.lines().filter(|line| !line.is_empty()).map(str::to_owned).collect()
}

/// The socket a launch uses (`<data dir>/openidea.sock` by default).
pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("openidea.sock")
}

#[cfg(test)]
mod tests {
    use super::split_arguments;

    #[test]
    fn a_handover_carries_one_argument_per_line() {
        assert_eq!(split_arguments("/a/project\n/b/file.rs\n"), vec!["/a/project", "/b/file.rs"]);
        assert!(split_arguments("").is_empty());
        assert!(split_arguments("\n\n").is_empty(), "blank lines are not arguments");
    }
}
