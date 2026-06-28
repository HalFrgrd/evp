//! Pseudo‑terminal helper.
//!
//! Adapted from the `ghostling_rs` example in libghostty‑rs. We `forkpty`
//! a child shell, put the master fd into non‑blocking mode and expose
//! `read`/`write`/`resize` plus the underlying fd for `poll(2)`.

#![allow(unsafe_code)]

use std::{
    ffi::OsString,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::Command,
};

use anyhow::{Context, Result, bail};
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
        mimic_vhs: bool,
    ) -> Result<(Self, Child)> {
        // Resolved command list and environment variables
        let mut shell_cmd = Vec::new();
        let mut extra_envs = Vec::new();

        if mimic_vhs {
            let shell_name = match shell {
                Some(raw) if !raw.trim().is_empty() => {
                    let raw = raw.trim();
                    let parts = parse_shell_command(raw)?;
                    if parts.len() != 1 {
                        bail!("shell command has more than one word: `{}`", raw);
                    }
                    parts[0].clone()
                }
                _ => "bash".to_string(),
            };

            let (cmd_args, extra_env) = match shell_name.as_str() {
                "bash" => (
                    vec!["bash".to_string(), "--noprofile".to_string(), "--norc".to_string(), "--login".to_string(), "+o".to_string(), "history".to_string()],
                    vec![("PS1".to_string(), "\\[\\e[38;2;90;86;224m\\]> \\[\\e[0m\\]".to_string()), ("BASH_SILENCE_DEPRECATION_WARNING".to_string(), "1".to_string())]
                ),
                "zsh" => (
                    vec!["zsh".to_string(), "--histnostore".to_string(), "--no-rcs".to_string()],
                    vec![("PROMPT".to_string(), "%F{#5B56E0}> %F{reset_color}".to_string())]
                ),
                "fish" => (
                    vec![
                        "fish".to_string(),
                        "--login".to_string(),
                        "--no-config".to_string(),
                        "--private".to_string(),
                        "-C".to_string(), "function fish_greeting; end".to_string(),
                        "-C".to_string(), "function fish_prompt; set_color 5B56E0; echo -n \"> \"; set_color normal; end".to_string()
                    ],
                    vec![]
                ),
                "powershell" => (
                    vec![
                        "powershell".to_string(),
                        "-NoLogo".to_string(),
                        "-NoExit".to_string(),
                        "-NoProfile".to_string(),
                        "-Command".to_string(),
                        "Set-PSReadLineOption -HistorySaveStyle SaveNothing; function prompt { Write-Host '>' -NoNewLine -ForegroundColor Blue; return ' ' }".to_string()
                    ],
                    vec![]
                ),
                "pwsh" => (
                    vec![
                        "pwsh".to_string(),
                        "-Login".to_string(),
                        "-NoLogo".to_string(),
                        "-NoExit".to_string(),
                        "-NoProfile".to_string(),
                        "-Command".to_string(),
                        "Set-PSReadLineOption -HistorySaveStyle SaveNothing; Function prompt { Write-Host -ForegroundColor Blue -NoNewLine '>'; return ' ' }".to_string()
                    ],
                    vec![]
                ),
                "cmd" => (
                    vec!["cmd.exe".to_string(), "/k".to_string(), "prompt=^> ".to_string()],
                    vec![]
                ),
                "nu" => (
                    vec!["nu".to_string(), "--execute".to_string(), "$env.PROMPT_COMMAND = {'\x1b[;38;2;91;86;224m>\x1b[m '}; $env.PROMPT_COMMAND_RIGHT = {''}".to_string()],
                    vec![]
                ),
                "osh" => (
                    vec!["osh".to_string(), "--norc".to_string()],
                    vec![("PS1".to_string(), "\\[\\e[38;2;90;86;224m\\]> \\[\\e[0m\\]".to_string())]
                ),
                "xonsh" => (
                    vec!["xonsh".to_string(), "--no-rc".to_string(), "-D".to_string(), "PROMPT=\x1b[;38;2;91;86;224m>\x1b[m ".to_string()],
                    vec![]
                ),
                other => bail!("invalid shell: {other}"),
            };
            shell_cmd = cmd_args;
            extra_envs = extra_env;
        } else {
            match shell {
                Some(raw) if !raw.trim().is_empty() => {
                    let parts = parse_shell_command(raw)?;
                    if parts.is_empty() {
                        let shell_path = default_shell_path();
                        shell_cmd.push(shell_path.to_string_lossy().to_string());
                    } else {
                        shell_cmd = parts;
                    }
                }
                _ => {
                    let shell_path = default_shell_path();
                    shell_cmd.push(shell_path.to_string_lossy().to_string());
                }
            }
        }

        let ws = size.to_winsize();
        match unsafe { nix::pty::forkpty(&ws, None) }.context("forkpty failed")? {
            ForkptyResult::Child => {
                let program = &shell_cmd[0];
                let mut cmd = Command::new(program);
                if shell_cmd.len() > 1 {
                    cmd.args(&shell_cmd[1..]);
                }
                cmd.arg0(command_arg0(program));

                cmd.env("TERM", "xterm-256color");

                let is_utf8_locale = |val: &str| -> bool {
                    let val_lower = val.to_lowercase();
                    val_lower.contains("utf-8") || val_lower.contains("utf8")
                };

                let get_effective_env = |name: &str| -> Option<String> {
                    if let Some((_, v)) = env.iter().find(|(k, _)| k == name) {
                        Some(v.clone())
                    } else {
                        std::env::var(name).ok()
                    }
                };

                let has_utf8 = {
                    if let Some(lc_all) = get_effective_env("LC_ALL") {
                        is_utf8_locale(&lc_all)
                    } else if let Some(lc_ctype) = get_effective_env("LC_CTYPE") {
                        is_utf8_locale(&lc_ctype)
                    } else if let Some(lang) = get_effective_env("LANG") {
                        is_utf8_locale(&lang)
                    } else {
                        false
                    }
                };

                if !has_utf8 {
                    cmd.env("LANG", "C.UTF-8");
                    cmd.env("LC_ALL", "C.UTF-8");
                }

                // Apply VHS-specific envs first
                for (k, v) in &extra_envs {
                    cmd.env(k, v);
                }

                // Apply user-specified envs next, which will override the VHS defaults if they match
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

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }

    pub fn try_clone(&self) -> Result<Self> {
        let fd = self
            .0
            .try_clone()
            .context("failed to clone PTY master fd")?;
        Ok(Self(fd))
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
    use super::*;

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

    #[test]
    fn test_pty_spawn_sets_utf8_locale_if_missing() {
        use super::{Pty, PtySize};

        let (pty, _child) = Pty::spawn(
            Some("printenv LANG"),
            &[],
            PtySize {
                cols: 80,
                rows: 24,
                px_w: 640,
                px_h: 380,
            },
            false,
        )
        .unwrap();

        let mut buf = [0u8; 1024];
        let mut output = String::new();
        let mut attempts = 0;
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(pty.fd()) };
        loop {
            match nix::unistd::read(&fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    output.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                Err(nix::errno::Errno::EAGAIN) => {
                    attempts += 1;
                    if attempts > 100 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }

        assert!(!output.is_empty());
        let val = output.trim();
        assert!(val.to_lowercase().contains("utf-8") || val.to_lowercase().contains("utf8"));
    }

    #[test]
    fn test_mimic_vhs_validation() {
        let size = PtySize {
            cols: 80,
            rows: 24,
            px_w: 640,
            px_h: 380,
        };

        // Multi-word shells are rejected
        let res = Pty::spawn(Some("bash --norc"), &[], size, true);
        match res {
            Err(e) => assert!(e.to_string().contains("more than one word")),
            _ => panic!("Expected error, got Ok"),
        }

        // Unsupported shells are rejected
        let res = Pty::spawn(Some("unknown_shell_xyz"), &[], size, true);
        match res {
            Err(e) => assert!(e.to_string().contains("invalid shell")),
            _ => panic!("Expected error, got Ok"),
        }

        // Supported shell (single-word) is accepted
        let res = Pty::spawn(Some("bash"), &[], size, true);
        assert!(res.is_ok());
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
            let _ = signal::kill(pid, signal::SIGKILL);
            let _ = wait::waitpid(pid, None);
        }
    }
}
