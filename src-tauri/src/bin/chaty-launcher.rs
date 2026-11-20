//! The portable launcher: the `chaty.exe` in the portable root, which starts
//! the program itself in `Program\` beside it and exits.
//!
//! The real app decides everything else — it finds the `portable` marker next
//! to its own folder (see `crate::portable`) and keeps its files in `Data\`.
//! This process carries no state and needs no configuration.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::ffi::OsString;
use std::process::Command;

fn main() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(root) = exe.parent() else {
        return;
    };
    let program = root.join("Program").join(app_name());
    if !program.is_file() {
        return;
    }
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // Fire and forget: the launcher has no window of its own, and waiting here
    // would leave a second process standing for as long as the app runs.
    let _ = Command::new(program).args(args).spawn();
}

fn app_name() -> &'static str {
    if cfg!(windows) {
        "chaty.exe"
    } else {
        "chaty"
    }
}
