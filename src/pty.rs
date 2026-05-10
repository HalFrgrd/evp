//! Pseudo‑terminal helper.
//!
//! Adapted from the `ghostling_rs` example in libghostty‑rs. We `forkpty`
//! a child shell, put the master fd into non‑blocking mode and expose
//! `read`/`write`/`resize` plus the underlying fd for `poll(2)`.

#![allow(unsafe_code)]

use std::{
    ffi::OsString,
    os::{
        fd::{AsRawFd, OwnedFd, RawFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::Command,
};

use anyhow::{Context, Result};
use libghostty_vt::Terminal;
use nix::{
    errno::Errno,
    fcntl::{self, OFlag},
    pty::{ForkptyResult, Winsize},
    sys::{signal, wait},
    unistd::{self, Pid},
};

/// Master side of the pseudo-terminal.
pub struct Pty(OwnedFd);

#[derive(Clone, Copy, Debug)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub px_w: u16,
    pub px_h: u16,
}

impl PtySize {
    fn to_winsize(self) -> Winsize {
        Winsize {
            ws_col: self.cols,
            ws_row: self.rows,
            ws_xpixel: self.px_w,
            ws_ypixel: self.px_h,
        }
    }
}

#[derive(Debug)]
pub enum PtyError {
    EndOfStream,
    Other(Errno),
}

impl std::fmt::Display for PtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PtyError::EndOfStream => write!(f, "pty closed"),
            PtyError::Other(e) => write!(f, "pty error: {e}"),
        }
    }
}
impl std::error::Error for PtyError {}

impl Pty {
    /// Spawn `shell` (or the user's default) in a fresh pseudo-terminal,
    /// applying `env` and an initial window `size`.
    pub fn spawn(
        shell: Option<&str>,
        env: &[(String, String)],
        size: PtySize,
    ) -> Result<(Self, Child)> {
        let ws = size.to_winsize();
        match unsafe { nix::pty::forkpty(&ws, None) }.context("forkpty failed")? {
            ForkptyResult::Child => {
                let mut cmd = match shell {
                    Some(raw) if !raw.trim().is_empty() => {
                        let parts = parse_shell_command(raw)?;
                        if parts.is_empty() {
                            let shell_path = default_shell_path();
                            let mut c = Command::new(&shell_path);
                            c.arg0(command_arg0(shell_path.as_os_str()));
                            c
                        } else {
                            let program = &parts[0];
                            let mut c = Command::new(program);
                            if parts.len() > 1 {
                                c.args(&parts[1..]);
                            }
                            c.arg0(command_arg0(program));
                            c
                        }
                    }
                    _ => {
                        let shell_path = default_shell_path();
                        let mut c = Command::new(&shell_path);
                        c.arg0(command_arg0(shell_path.as_os_str()));
                        c
                    }
                };
                cmd.env("TERM", "xterm-256color");
                for (k, v) in env {
                    cmd.env(k, v);
                }
                let _ = cmd.exec();
                std::process::exit(127);
            }
            ForkptyResult::Parent { child, master: fd } => {
                // Non‑blocking so reads return EAGAIN when the kernel buffer
                // drains rather than blocking the scheduler loop.
                let raw_flags = fcntl::fcntl(&fd, fcntl::F_GETFL)?;
                let flags = OFlag::from_bits_retain(raw_flags) | OFlag::O_NONBLOCK;
                fcntl::fcntl(&fd, fcntl::F_SETFL(flags))?;
                Ok((Self(fd), Child::Active(child)))
            }
        }
    }

    pub fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    /// Drain all currently available output into the terminal's VT parser.
    pub fn drain_into(&self, term: &mut Terminal<'_, '_>) -> Result<(), PtyError> {
        let mut buf = [0u8; 8192];
        loop {
            match nix::unistd::read(&self.0, &mut buf) {
                Ok(0) => return Err(PtyError::EndOfStream),
                Ok(n) => term.vt_write(&buf[..n]),
                Err(Errno::EAGAIN) => return Ok(()),
                Err(Errno::EINTR) => continue,
                Err(Errno::EIO) => return Err(PtyError::EndOfStream),
                Err(e) => return Err(PtyError::Other(e)),
            }
        }
    }

    /// Best-effort write – drops trailing data on EAGAIN to match the
    /// behaviour of typical terminal emulators under back-pressure.
    pub fn write(&self, mut data: &[u8]) {
        while !data.is_empty() {
            match nix::unistd::write(&self.0, data) {
                Ok(n) => data = &data[n..],
                Err(Errno::EINTR) => continue,
                Err(_) => break,
            }
        }
    }

    pub fn resize(&self, size: PtySize) {
        nix::ioctl_write_ptr_bad!(tiocswinsz, nix::libc::TIOCSWINSZ, Winsize);
        let ws = size.to_winsize();
        let _ = unsafe { tiocswinsz(self.0.as_raw_fd(), &ws) };
    }
}

fn default_shell_path() -> PathBuf {
    match std::env::var_os("SHELL") {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        _ => match unistd::User::from_uid(unistd::getuid()) {
            Ok(Some(user)) => user.shell,
            _ => PathBuf::from("/bin/sh"),
        },
    }
}

fn command_arg0(program: impl AsRef<std::ffi::OsStr>) -> OsString {
    PathBuf::from(program.as_ref())
        .file_name()
        .map(|s| s.to_owned())
        .unwrap_or_else(|| program.as_ref().to_owned())
}

fn parse_shell_command(input: &str) -> Result<Vec<String>> {
    shell_words::split(input).with_context(|| format!("invalid shell command: `{input}`"))
}

#[cfg(test)]
mod tests {
    use super::parse_shell_command;

    #[test]
    fn parse_shell_command_handles_shell_forms() {
        assert_eq!(
            parse_shell_command("bash").unwrap(),
            vec!["bash".to_string()]
        );
        assert_eq!(
            parse_shell_command("/bin/bash").unwrap(),
            vec!["/bin/bash".to_string()]
        );
        assert_eq!(
            parse_shell_command("bash --norc").unwrap(),
            vec!["bash".to_string(), "--norc".to_string()]
        );
        assert_eq!(
            parse_shell_command("bash --rcfile somefile.rc").unwrap(),
            vec![
                "bash".to_string(),
                "--rcfile".to_string(),
                "somefile.rc".to_string()
            ]
        );
        assert_eq!(
            parse_shell_command("fish").unwrap(),
            vec!["fish".to_string()]
        );
        assert_eq!(parse_shell_command("sh").unwrap(), vec!["sh".to_string()]);
        assert_eq!(
            parse_shell_command("/bin/sh").unwrap(),
            vec!["/bin/sh".to_string()]
        );
    }
}

/// Lifecycle wrapper that ensures we reap the child on drop.
pub enum Child {
    Active(Pid),
    Reaped,
}

impl Drop for Child {
    fn drop(&mut self) {
        if let Child::Active(pid) = *self {
            let _ = signal::kill(pid, signal::SIGHUP);
            let _ = wait::waitpid(pid, None);
        }
    }
}
