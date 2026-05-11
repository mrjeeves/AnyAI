//! Subprocess spawn helpers.
//!
//! Every external process MyOwnLLM launches (ollama, nvidia-smi, tar, curl, …)
//! must go through these helpers. On Windows, `Command::new` inherits the
//! parent's "subsystem" decision: a GUI-subsystem parent (the release build,
//! see `windows_subsystem = "windows"`) has no console, so each child opens
//! its own — a black CMD window flashes for every spawn. Settings tab
//! navigation that calls into `detect_hardware`, `ollama_list_models`, etc.
//! produces a visible storm of these flashes on Windows.
//!
//! `CREATE_NO_WINDOW` (0x0800_0000) tells Windows not to allocate a console
//! for the child process while still letting it inherit our stdio handles, so
//! captured output (`.output()`) keeps working unchanged. No-op on Unix.

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Drop-in replacement for `std::process::Command::new` that does not flash a
/// console window on Windows.
pub fn quiet_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    apply_quiet_flags(&mut cmd);
    cmd
}

/// Drop-in replacement for `tokio::process::Command::new` that does not flash
/// a console window on Windows.
pub fn quiet_tokio_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    apply_quiet_flags_tokio(&mut cmd);
    cmd
}

#[cfg(target_os = "windows")]
fn apply_quiet_flags(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(target_os = "windows"))]
fn apply_quiet_flags(_cmd: &mut std::process::Command) {}

#[cfg(target_os = "windows")]
fn apply_quiet_flags_tokio(cmd: &mut tokio::process::Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(target_os = "windows"))]
fn apply_quiet_flags_tokio(_cmd: &mut tokio::process::Command) {}
