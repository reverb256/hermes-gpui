//! Single-instance hand-off for `--toggle`.
//!
//! The first process owns a per-user Unix socket in `XDG_RUNTIME_DIR`; later
//! launches connect to it and ask the running window to show or toggle instead
//! of starting a second gateway client. The socket is mode 0600 and is removed
//! on a clean exit.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Toggle,
    Show,
}

pub enum Claim {
    /// This process owns the socket and should run the app.
    Primary(Listener),
    /// Another process is already listening; forward and exit.
    Secondary,
    /// The socket could not be created; run without single-instance handling.
    Unavailable,
}

pub struct Listener {
    path: PathBuf,
    socket: UnixListener,
}

impl Listener {
    /// Non-blocking accepts for the GPUI poll loop.
    pub fn poll(&self) -> Vec<Request> {
        let mut requests = Vec::new();
        loop {
            match self.socket.accept() {
                Ok((mut stream, _)) => {
                    // A connection can arrive before its first byte. Give the
                    // local sender a brief window to finish its small request.
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(50)));
                    let mut buffer = [0_u8; 64];
                    match stream.read(&mut buffer) {
                        Ok(read) if read > 0 => {
                            if let Some(request) =
                                parse_request(std::str::from_utf8(&buffer[..read]).unwrap_or(""))
                            {
                                requests.push(request);
                            }
                        }
                        Ok(_) | Err(_) => {}
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => {
                    tracing::warn!(%err, "ipc accept failed");
                    break;
                }
            }
        }
        requests
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Take the singleton socket, replacing a stale one left by a crash.
pub fn claim() -> Claim {
    let path = socket_path();
    match bind(&path) {
        Ok(socket) => Claim::Primary(Listener { path, socket }),
        Err(err) if err.kind() == ErrorKind::AddrInUse => {
            // A live process answers; a crash leaves a dead inode behind.
            match UnixStream::connect(&path) {
                Ok(_) => return Claim::Secondary,
                Err(err) if err.kind() == ErrorKind::ConnectionRefused => {}
                Err(err) => {
                    tracing::warn!(%err, "cannot verify whether ipc socket is stale");
                    return Claim::Unavailable;
                }
            }
            use std::os::unix::fs::FileTypeExt;
            if !std::fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.file_type().is_socket())
            {
                tracing::warn!("ipc path is not a socket");
                return Claim::Unavailable;
            }
            if let Err(err) = std::fs::remove_file(&path) {
                tracing::warn!(%err, "cannot remove stale ipc socket");
                return Claim::Unavailable;
            }
            match bind(&path) {
                Ok(socket) => Claim::Primary(Listener { path, socket }),
                Err(err) => {
                    tracing::warn!(%err, "ipc socket unavailable");
                    Claim::Unavailable
                }
            }
        }
        Err(err) => {
            tracing::warn!(%err, "ipc socket unavailable");
            Claim::Unavailable
        }
    }
}

/// Send one request to the running instance. Returns false when nothing is
/// listening, which is the normal startup path for the first launch.
pub fn forward(request: Request) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket_path()) else {
        return false;
    };
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(500)));
    stream.write_all(request.as_str().as_bytes()).is_ok() && stream.flush().is_ok()
}

impl Request {
    fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle\n",
            Self::Show => "show\n",
        }
    }
}

fn parse_request(text: &str) -> Option<Request> {
    match text.trim() {
        "toggle" => Some(Request::Toggle),
        "show" => Some(Request::Show),
        _ => None,
    }
}

fn bind(path: &PathBuf) -> std::io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let socket = UnixListener::bind(path)?;
    socket.set_nonblocking(true)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(socket)
}

fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("HERMES_GPUI_SOCKET") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let user: String = user
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    base.join(format!("hermes-gpui-{user}.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_two_requests_and_ignores_noise() {
        assert_eq!(parse_request("toggle\n"), Some(Request::Toggle));
        assert_eq!(parse_request(" show "), Some(Request::Show));
        assert_eq!(parse_request("quit"), None);
        assert_eq!(parse_request(""), None);
    }
}
