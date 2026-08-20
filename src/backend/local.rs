use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

#[cfg(not(windows))]
use std::time::Instant;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
#[cfg(not(windows))]
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::session::config::LocalTerminalShell;
use crate::terminal::{BackendCommand, BackendEvent, BackendTx, GuardedBackendEventSender};

#[cfg(not(windows))]
const DIRECTORY_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[cfg(not(windows))]
fn local_process_directory(system: &mut System, pid: Pid) -> Option<std::path::PathBuf> {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always),
    );
    system
        .process(pid)
        .and_then(|process| process.cwd())
        .map(std::path::PathBuf::from)
}

#[derive(Debug, Clone)]
pub struct LocalTerminalShellLaunch {
    pub executable: PathBuf,
}

/// Resolves a configured local shell to an executable available on this host.
pub fn resolve_local_terminal_shell(
    shell: LocalTerminalShell,
) -> anyhow::Result<LocalTerminalShellLaunch> {
    #[cfg(not(windows))]
    let _ = shell;

    #[cfg(windows)]
    {
        let executable = match shell {
            LocalTerminalShell::WindowsPowerShell => resolve_windows_powershell(),
            LocalTerminalShell::PowerShell7 => resolve_powershell7(),
            LocalTerminalShell::CommandPrompt => resolve_command_prompt(),
            LocalTerminalShell::GitBash => resolve_git_bash(),
        }
        .ok_or_else(|| anyhow::anyhow!("configured local shell is not installed: {shell:?}"))?;
        return Ok(LocalTerminalShellLaunch { executable });
    }

    #[cfg(not(windows))]
    {
        let executable = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/zsh"));
        Ok(LocalTerminalShellLaunch { executable })
    }
}

pub fn local_terminal_shell_available(shell: LocalTerminalShell) -> bool {
    resolve_local_terminal_shell(shell).is_ok()
}

#[cfg(windows)]
fn existing_file(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

#[cfg(windows)]
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

#[cfg(windows)]
fn find_on_path(name: &str, predicate: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file() && predicate(candidate))
}

#[cfg(windows)]
fn resolve_windows_powershell() -> Option<PathBuf> {
    let system_root = env_path("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    existing_file(system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
        .or_else(|| find_on_path("powershell.exe", |_| true))
}

#[cfg(windows)]
fn resolve_powershell7() -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(program_files) = env_path("ProgramFiles") {
        roots.push(program_files.join("PowerShell"));
    }
    if let Some(program_files) = env_path("ProgramW6432") {
        roots.push(program_files.join("PowerShell"));
    }
    if let Some(program_files) = env_path("ProgramFiles(x86)") {
        roots.push(program_files.join("PowerShell"));
    }
    if let Some(local_app_data) = env_path("LOCALAPPDATA") {
        roots.push(local_app_data.join(r"Programs\PowerShell"));
    }

    let mut candidates = Vec::new();
    if let Some(local_app_data) = env_path("LOCALAPPDATA") {
        candidates.push(local_app_data.join(r"Microsoft\WindowsApps\pwsh.exe"));
    }
    if let Some(scoop) = env_path("SCOOP") {
        candidates.push(scoop.join(r"apps\powershell\current\pwsh.exe"));
    }
    if let Some(user_profile) = env_path("USERPROFILE") {
        candidates.push(user_profile.join(r"scoop\apps\powershell\current\pwsh.exe"));
    }
    for root in roots {
        if let Some(candidate) = existing_file(root.join("pwsh.exe")) {
            candidates.push(candidate);
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut versions = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.')
            })
            .collect::<Vec<_>>();
        versions.sort_by_key(|entry| entry.file_name());
        candidates.extend(
            versions
                .into_iter()
                .rev()
                .filter_map(|entry| existing_file(entry.path().join("pwsh.exe"))),
        );
    }

    candidates
        .into_iter()
        .next()
        .or_else(|| find_on_path("pwsh.exe", |_| true))
}

#[cfg(windows)]
fn resolve_command_prompt() -> Option<PathBuf> {
    env_path("ComSpec")
        .and_then(existing_file)
        .or_else(|| {
            let system_root =
                env_path("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            existing_file(system_root.join(r"System32\cmd.exe"))
        })
        .or_else(|| find_on_path("cmd.exe", |_| true))
}

#[cfg(windows)]
fn resolve_git_bash() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for variable in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(program_files) = env_path(variable) {
            candidates.push(program_files.join(r"Git\bin\bash.exe"));
        }
    }
    if let Some(local_app_data) = env_path("LOCALAPPDATA") {
        candidates.push(local_app_data.join(r"Programs\Git\bin\bash.exe"));
    }
    if let Some(scoop) = env_path("SCOOP") {
        candidates.push(scoop.join(r"apps\git\current\bin\bash.exe"));
    }
    if let Some(user_profile) = env_path("USERPROFILE") {
        candidates.push(user_profile.join(r"scoop\apps\git\current\bin\bash.exe"));
    }

    candidates.into_iter().find_map(existing_file).or_else(|| {
        find_on_path("bash.exe", |candidate| {
            let path = candidate.to_string_lossy().to_ascii_lowercase();
            path.contains("\\git\\") || path.contains("/git/")
        })
    })
}

pub fn spawn_local_terminal_at(
    tab_id: String,
    cols: u16,
    rows: u16,
    events: GuardedBackendEventSender,
    initial_directory: Option<&Path>,
    shell: LocalTerminalShell,
) -> Result<BackendTx> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("open local PTY")?;

    let launch = resolve_local_terminal_shell(shell).context("resolve local shell")?;
    let mut cmd = CommandBuilder::new(&launch.executable);
    #[cfg(windows)]
    {
        const POWERSHELL_CWD_REPORTER: &str = r#"& {
            $global:AshellOriginalPrompt = $function:prompt
            function global:prompt {
                $promptText = if ($global:AshellOriginalPrompt) { & $global:AshellOriginalPrompt } else { "PS $PWD> " }
                $cwd = $PWD.ProviderPath
                $encoded = [Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($cwd))
                [Console]::Write("$([char]27)]133;D$([char]7)$([char]27)]133;A$([char]7)")
                [Console]::Write("$([char]27)]0;ASHELL_CWD_B64:$encoded$([char]7)")
                "$promptText$([char]27)]133;B$([char]7)"
            }
        }"#;
        match shell {
            LocalTerminalShell::WindowsPowerShell | LocalTerminalShell::PowerShell7 => {
                cmd.args(["-NoLogo", "-NoExit", "-Command", POWERSHELL_CWD_REPORTER]);
            }
            LocalTerminalShell::CommandPrompt => {
                let original_prompt =
                    std::env::var("PROMPT").unwrap_or_else(|_| "$P$G".to_string());
                let prompt = format!(
                    "\x1b]133;D\x07\x1b]133;A\x07\x1b]0;ASHELL_CWD:$P\x07{original_prompt}\x1b]133;B\x07"
                );
                cmd.args(["/Q"]);
                cmd.env("PROMPT", prompt);
            }
            LocalTerminalShell::GitBash => {
                let reporter = r#"printf '\033]133;D\a\033]133;A\a\033]0;ASHELL_CWD:%s\a' "$(pwd -W 2>/dev/null || pwd)""#;
                let original_prompt_command = std::env::var("PROMPT_COMMAND").ok();
                let prompt_command = match original_prompt_command {
                    Some(command) if !command.trim().is_empty() => {
                        format!("{reporter};{command}")
                    }
                    _ => reporter.to_string(),
                };
                cmd.args(["--login", "-i"]);
                cmd.env("CHERE_INVOKING", "1");
                cmd.env("PROMPT_COMMAND", prompt_command);
            }
        }
    }
    cmd.env(
        "TERM",
        std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
    );
    cmd.env(
        "COLORTERM",
        std::env::var("COLORTERM").unwrap_or_else(|_| "truecolor".into()),
    );
    cmd.env("TERM_PROGRAM", "ashell");
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    if let Ok(lang) = std::env::var("LANG") {
        cmd.env("LANG", lang);
    } else {
        cmd.env("LANG", "en_US.UTF-8");
    }
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    if let Some(directory) = initial_directory.filter(|path| path.is_dir()) {
        cmd.cwd(directory.as_os_str());
    }
    cmd.env("SHELL", launch.executable.as_os_str());
    let mut child = pair.slave.spawn_command(cmd).context("spawn local shell")?;
    #[cfg(not(windows))]
    let child_pid = child.process_id().map(Pid::from_u32);
    drop(pair.slave);

    let master = pair.master;
    let mut reader = master.try_clone_reader().context("clone PTY reader")?;
    let mut writer = master.take_writer().context("take PTY writer")?;
    let (cmd_tx, cmd_rx) = mpsc::channel::<BackendCommand>();

    let read_tab = tab_id.clone();
    let read_events = events.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = read_events.send(BackendEvent::Output {
                        tab_id: read_tab.clone(),
                        bytes: buf[..n].to_vec(),
                    });
                }
                Err(err) => {
                    let _ = read_events.send(BackendEvent::Closed {
                        tab_id: read_tab.clone(),
                        reason: format!("local read error: {err}"),
                    });
                    return;
                }
            }
        }
        let _ = read_events.send(BackendEvent::Closed {
            tab_id: read_tab,
            reason: "local shell closed".into(),
        });
    });

    let write_tab = tab_id.clone();
    let write_events = events.clone();
    thread::spawn(move || {
        #[cfg(not(windows))]
        let mut process_system = System::new();
        #[cfg(not(windows))]
        let mut last_directory = None;
        #[cfg(not(windows))]
        let mut last_directory_check = None;

        loop {
            match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => match command {
                    BackendCommand::Input(bytes) => {
                        if let Err(err) = writer.write_all(&bytes) {
                            let _ = write_events.send(BackendEvent::Closed {
                                tab_id: write_tab.clone(),
                                reason: format!("local write error: {err}"),
                            });
                            break;
                        }
                        let _ = writer.flush();
                    }
                    BackendCommand::Resize { cols, rows } => {
                        let _ = master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    BackendCommand::Close => break,
                    BackendCommand::SampleMetrics
                    | BackendCommand::SampleProcesses
                    | BackendCommand::SamplePorts
                    | BackendCommand::TerminateProcess { .. } => {}
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        let _ = write_events.send(BackendEvent::Closed {
                            tab_id: write_tab,
                            reason: format!("local shell exited: {status}"),
                        });
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            #[cfg(not(windows))]
            {
                if last_directory_check.is_none_or(|checked_at: Instant| {
                    checked_at.elapsed() >= DIRECTORY_POLL_INTERVAL
                }) {
                    last_directory_check = Some(Instant::now());
                    if let Some(directory) =
                        child_pid.and_then(|pid| local_process_directory(&mut process_system, pid))
                    {
                        if last_directory.as_ref() != Some(&directory) {
                            last_directory = Some(directory.clone());
                            let _ = write_events.send(BackendEvent::LocalDirectoryChanged {
                                tab_id: write_tab.clone(),
                                path: directory,
                            });
                        }
                    }
                }
            }
        }
        let _ = child.kill();
    });

    let _ = events.send(BackendEvent::Status {
        tab_id,
        text: "local shell ready".into(),
    });

    Ok(BackendTx::Local(cmd_tx))
}
