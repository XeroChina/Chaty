//! The portable layout.
//!
//! A zip of the Windows build unpacks to a folder of the user's choosing:
//!
//! ```text
//! Chaty\
//!   chaty.exe      launcher — starts Program\chaty.exe and exits
//!   portable       the marker file; its presence turns portable mode on
//!   Program\       the program: chaty.exe, the engine sidecars, the DLLs
//!   Data\          everything the app writes: settings, chats, models, logs
//! ```
//!
//! The marker is looked for beside the program's own folder, so the layout
//! works whichever of the two exes was double-clicked — the launcher or the
//! program itself. Everywhere else (a normal install, `tauri dev`) there is no
//! marker and the app keeps using Tauri's app-data dir.

use std::path::{Path, PathBuf};

use tauri::Manager as _;

/// The portable root — the folder holding `portable`, `Program` and `Data` —
/// when this process runs from one. `None` in a normal install and in dev.
pub fn root() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|exe| root_at(&exe))
}

/// `root` for a given executable path, split out so it can be tested without
/// moving the real executable around.
fn root_at(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let root = match dir.file_name().and_then(|n| n.to_str()) {
        // `…\Chaty\Program\chaty.exe` — one level up is the portable root.
        Some(name) if name.eq_ignore_ascii_case("Program") => dir.parent()?,
        // `…\Chaty\chaty.exe`, or a program that happens to sit in its own
        // portable root: the folder it is in.
        _ => dir,
    };
    if root.join("portable").is_file() {
        Some(root.to_path_buf())
    } else {
        None
    }
}

/// The portable `Data\` folder, when this process runs from a portable layout.
pub fn portable_data_dir() -> Option<PathBuf> {
    let dir = root()?.join("Data");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// Every user file's home: the portable `Data\` folder in a portable layout,
/// Tauri's app-data dir otherwise.
pub fn data_dir(app: &tauri::AppHandle) -> PathBuf {
    if let Some(dir) = portable_data_dir() {
        return dir;
    }
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("com.chaty.desktop"))
}

#[cfg(test)]
mod tests {
    use super::root_at;
    use std::path::Path;

    #[test]
    fn the_program_inside_program_finds_the_root_a_level_up() {
        let root = Path::new("D:/Apps/Chaty");
        assert_eq!(
            root_at(&root.join("Program").join("chaty.exe")).as_deref(),
            Some(root)
        );
        // A folder name of any case is the same folder on Windows.
        assert_eq!(
            root_at(&root.join("program").join("chaty.exe")).as_deref(),
            Some(root)
        );
    }

    #[test]
    fn an_exe_in_the_root_finds_that_root() {
        let root = Path::new("D:/Apps/Chaty");
        assert_eq!(root_at(&root.join("chaty.exe")).as_deref(), Some(root));
    }

    #[test]
    fn a_normal_install_has_no_portable_root() {
        // No `portable` marker next to the exe: the layout is not portable, so
        // the path is rejected even though it sits in a folder of its own.
        let dir = std::env::temp_dir().join(format!("chaty-portable-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Program")).unwrap();
        std::fs::write(dir.join("Program").join("chaty.exe"), b"").unwrap();
        assert_eq!(root_at(&dir.join("Program").join("chaty.exe")), None);

        // Drop the marker in: the same layout is portable now.
        std::fs::write(dir.join("portable"), b"").unwrap();
        assert_eq!(root_at(&dir.join("Program").join("chaty.exe")).as_deref(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
