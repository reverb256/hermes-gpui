//! Pseudo-terminal sessions for terminal commands (Unix only).
//!
//! A terminal command is a normal command with a `PtySpec`: the child gets the
//! slave side as its controlling terminal, its output is relayed on
//! `StreamChannel::Pty`, and input/resize frames are applied until it exits or
//! is cancelled. Input bytes are written straight to the master and never
//! logged, so a typed password stays inside the session.
//!
//! Everything here uses `libc` directly instead of a terminal crate: the node
//! only needs `openpty`-equivalent calls, `TIOCSCTTY`, `TIOCSWINSZ` and a hangup
//! of the child's process group.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hermes_protocol::{CommandStatus, NodeToHub, PtySpec, StreamChannel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::sync::{mpsc, watch};

use crate::TerminalControl;

/// One read from the master, and therefore one stream frame.
const PTY_CHUNK: usize = 8 * 1024;
/// How long a hung-up session gets to exit before it is killed outright.
const HANGUP_GRACE: Duration = Duration::from_secs(2);
/// What a terminal command asks the node to run.
pub struct Job<'a> {
    pub command_id: String,
    pub argv: &'a [String],
    pub cwd: &'a Path,
    pub env: &'a BTreeMap<String, String>,
    pub spec: PtySpec,
}

/// Run one terminal command to completion, streaming its output to the hub.
pub async fn run(
    job: Job<'_>,
    tx: mpsc::UnboundedSender<NodeToHub>,
    mut control: mpsc::UnboundedReceiver<TerminalControl>,
    mut cancel: watch::Receiver<bool>,
) -> io::Result<(CommandStatus, Option<i32>)> {
    let Job {
        command_id,
        argv,
        cwd,
        env,
        spec,
    } = job;
    let (mut child, reader, mut writer, size_fd) =
        Session::open(argv, cwd, env, spec)?.into_parts();
    let _ = tx.send(NodeToHub::CommandStarted {
        command_id: command_id.clone(),
        at_ms: crate::now_ms(),
        pid: child.id(),
    });

    let seq = Arc::new(AtomicU64::new(1));
    let reader_tx = tx.clone();
    let reader_id = command_id.clone();
    let reader = tokio::spawn(async move {
        let mut master = reader;
        let mut buffer = vec![0u8; PTY_CHUNK];
        loop {
            match master.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => {
                    let seq = seq.fetch_add(1, Ordering::Relaxed);
                    let frame = NodeToHub::Stream {
                        command_id: reader_id.clone(),
                        seq,
                        channel: StreamChannel::Pty,
                        data_b64: STANDARD.encode(&buffer[..read]),
                    };
                    if reader_tx.send(frame).is_err() {
                        break;
                    }
                }
                // EIO is how a Linux master reports that the slave is gone.
                Err(_) => break,
            }
        }
    });

    let outcome = loop {
        tokio::select! {
            exit = child.wait() => {
                break match exit {
                    Ok(status) => (
                        if status.success() { CommandStatus::Succeeded } else { CommandStatus::Failed },
                        status.code(),
                    ),
                    Err(err) => {
                        tracing::warn!(error = %err, "wait for terminal child failed");
                        (CommandStatus::Failed, None)
                    }
                };
            }
            _ = cancel.changed() => {
                hang_up(&mut child).await;
                break (CommandStatus::Cancelled, None);
            }
            message = control.recv() => {
                match message {
                    Some(TerminalControl::Input(data)) => {
                        // A closed master means the session is already over;
                        // the wait branch above reports the real status.
                        let _ = writer.write_all(&data).await;
                        let _ = writer.flush().await;
                    }
                    Some(TerminalControl::Resize(next)) if next.validate().is_ok() => {
                        let _ = set_size(size_fd.as_raw_fd(), next);
                    }
                    _ => {}
                }
            }
        }
    };

    // Let the reader drain what the child wrote before it exited.
    let _ = tokio::time::timeout(HANGUP_GRACE, reader).await;
    Ok(outcome)
}

struct Session {
    child: Child,
    reader: tokio::fs::File,
    writer: tokio::fs::File,
    size_fd: OwnedFd,
}

impl Session {
    fn open(
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        spec: PtySpec,
    ) -> io::Result<Self> {
        let (master, slave) = open_pair()?;
        set_size(master.as_raw_fd(), spec)?;
        let stdin = slave.try_clone()?;
        let stdout = slave.try_clone()?;
        let stderr = slave.try_clone()?;

        let mut process = tokio::process::Command::new(&argv[0]);
        process
            .args(&argv[1..])
            .current_dir(cwd)
            .env_clear()
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true);
        for (key, value) in env {
            process.env(key, value);
        }
        // The child leads its own session and takes the slave as controlling
        // terminal, so job control (Ctrl+C, Ctrl+Z, `fg`) behaves like a real
        // terminal instead of signalling nothing.
        unsafe {
            process.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = process.spawn()?;
        // Only the child keeps the slave open; the parent talks to the master.
        drop(slave);

        let read_fd = master.try_clone()?;
        let write_fd = master.try_clone()?;
        Ok(Self {
            child,
            reader: tokio::fs::File::from_std(std::fs::File::from(read_fd)),
            writer: tokio::fs::File::from_std(std::fs::File::from(write_fd)),
            size_fd: master,
        })
    }

    fn into_parts(self) -> (Child, tokio::fs::File, tokio::fs::File, OwnedFd) {
        (self.child, self.reader, self.writer, self.size_fd)
    }
}

/// Hang up the terminal so the shell and every job it started exit, then kill
/// the child if it ignores the hangup.
async fn hang_up(child: &mut Child) {
    if let Some(pid) = child.id() {
        // `setsid` in the child made its pid its process-group id.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGHUP);
        }
    }
    if tokio::time::timeout(HANGUP_GRACE, child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// Open a master/slave pair and leave the slave unlocked for `open`.
fn open_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    unsafe {
        let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = OwnedFd::from_raw_fd(fd);
        if libc::grantpt(master.as_raw_fd()) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::unlockpt(master.as_raw_fd()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut name = [0 as libc::c_char; 256];
        if libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let path = CStr::from_ptr(name.as_ptr());
        let path = CString::new(path.to_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "pty name contains NUL"))?;
        let slave = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        if slave < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((master, OwnedFd::from_raw_fd(slave)))
    }
}

fn set_size(fd: RawFd, spec: PtySpec) -> io::Result<()> {
    let size = libc::winsize {
        ws_row: spec.rows,
        ws_col: spec.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &size) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PtySpec {
        PtySpec {
            cols: 100,
            rows: 30,
        }
    }

    fn env() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin".to_string(),
            ),
            ("TERM".to_string(), "xterm-256color".to_string()),
        ])
    }

    fn command(argv: &[&str]) -> Vec<String> {
        argv.iter().map(|arg| arg.to_string()).collect()
    }

    #[tokio::test]
    async fn terminal_streams_output_and_reports_success() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let argv = command(&["sh", "-c", "printf pty-ok; exit 0"]);
        let cwd = std::env::temp_dir();
        let outcome = run(
            Job {
                command_id: "cmd-pty".into(),
                argv: &argv,
                cwd: &cwd,
                env: &env(),
                spec: spec(),
            },
            tx,
            control_rx,
            cancel_rx,
        )
        .await
        .expect("terminal session");
        assert_eq!(outcome, (CommandStatus::Succeeded, Some(0)));

        let mut output = String::new();
        while let Ok(frame) = rx.try_recv() {
            if let NodeToHub::Stream {
                channel: StreamChannel::Pty,
                data_b64,
                ..
            } = frame
            {
                output.push_str(&String::from_utf8_lossy(
                    &STANDARD.decode(data_b64).unwrap(),
                ));
            }
        }
        assert!(output.contains("pty-ok"), "{output:?}");
    }

    #[tokio::test]
    async fn terminal_input_reaches_the_child_and_cancel_hangs_up() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let argv = command(&["sh", "-c", "read line; printf 'got:%s' \"$line\"; sleep 30"]);
        let cwd = std::env::temp_dir();
        let session = tokio::spawn(async move {
            run(
                Job {
                    command_id: "cmd-pty".into(),
                    argv: &argv,
                    cwd: &cwd,
                    env: &env(),
                    spec: spec(),
                },
                tx,
                control_rx,
                cancel_rx,
            )
            .await
        });

        // The echo of the input only happens once the pty is live; poll until
        // the started frame arrives, then type a line.
        let started = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = rx.recv().await {
                if matches!(frame, NodeToHub::CommandStarted { .. }) {
                    return;
                }
            }
        })
        .await;
        assert!(started.is_ok(), "terminal never started");
        control_tx
            .send(TerminalControl::Input(b"secret-line\n".to_vec()))
            .expect("input");
        control_tx
            .send(TerminalControl::Resize(PtySpec { cols: 80, rows: 24 }))
            .expect("resize");

        let mut seen = String::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = rx.recv().await {
                if let NodeToHub::Stream { data_b64, .. } = frame {
                    seen.push_str(&String::from_utf8_lossy(
                        &STANDARD.decode(data_b64).unwrap(),
                    ));
                    if seen.contains("got:secret-line") {
                        return;
                    }
                }
            }
        })
        .await
        .expect("terminal echoed the input");
        assert!(seen.contains("got:secret-line"), "{seen:?}");

        cancel_tx.send(true).expect("cancel");
        let outcome = tokio::time::timeout(Duration::from_secs(10), session)
            .await
            .expect("session ended after cancel")
            .expect("join")
            .expect("session result");
        assert_eq!(outcome.0, CommandStatus::Cancelled);
    }
}
