//! File association for `.deemopack`, best-effort and silent.
//!
//! On startup (background thread, after the window exists) the game points the
//! per-user file association at its *current* executable. This is idempotent:
//! if the stored command already matches, nothing is rewritten.
//!
//! * Windows: `HKCU\Software\Classes\.deemopack` → `DeemoPack`,
//!   `...\DeemoPack\shell\open\command` → `"<exe>" "%1"`; no admin, no console.
//! * Linux: `~/.local/share/mime/packages/deemo-deemopack.xml` and
//!   `~/.local/share/applications/deemo.desktop` with
//!   `Exec=<absolute exe> %f`; updates the MIME/desktop caches when they change.
//!
//! Every failure is swallowed: read-only home, missing `xdg-mime`, unknown OS.
//! `ASSOC-WINDOWS.bat` and `scripts/install-desktop.sh` remain as manual repair
//! tools, but the normal path needs no script.

use std::path::{Path, PathBuf};

/// Human-readable result for the startup log.
pub fn register() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    {
        return windows_register(&exe);
    }
    #[cfg(target_os = "linux")]
    {
        let data = data_home().ok_or_else(|| "no XDG_DATA_HOME/HOME".to_string())?;
        let changed = linux_install(&data, &exe, true)?;
        return Ok(if changed {
            format!("assoc: registered .deemopack -> {}", exe.display())
        } else {
            "assoc: up to date".to_string()
        });
    }
    #[allow(unreachable_code)]
    {
        let _ = exe;
        Err("assoc: unsupported OS".to_string())
    }
}

/// Write the startup result where it can be read later: `data/deemo.log` on
/// Windows, stderr elsewhere. Association failures never reach the UI.
pub fn log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("data/deemo.log")
    {
        let _ = writeln!(f, "{msg}");
    }
    // Windows release already redirects stderr to its own log; on Linux the
    // file above is the one a file-manager launch can find.
    #[cfg(not(target_os = "windows"))]
    {
        eprintln!("{msg}");
    }
}

/// `$XDG_DATA_HOME` or `$HOME/.local/share`.
fn data_home() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x));
        }
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
}

/// Desktop `Exec=` value: absolute path, quoted when it contains whitespace.
fn desktop_exec(exe: &Path) -> String {
    let p = exe.to_string_lossy();
    if p.chars().any(char::is_whitespace) {
        format!("\"{p}\"")
    } else {
        p.into_owned()
    }
}

/// Directory that contains the game's `data/`. In a release it is next to
/// the executable; in a `cargo build` tree it is the working directory the
/// game is started from (otherwise `Path=` would point at `target/release`,
/// which has no assets).
#[cfg(target_os = "linux")]
fn game_root(exe: &Path) -> PathBuf {
    let exe_dir = exe.parent().unwrap_or_else(|| Path::new("."));
    let has_game_data = |dir: &Path| dir.join("data/library.json").is_file();
    if has_game_data(exe_dir) {
        return exe_dir.to_path_buf();
    }
    if let Ok(cwd) = std::env::current_dir() {
        if has_game_data(&cwd) {
            return cwd;
        }
    }
    exe_dir.to_path_buf()
}

#[cfg(target_os = "linux")]
fn desktop_file(exe: &Path) -> String {
    let root = game_root(exe);
    let path = root.to_string_lossy();
    let path = if path.chars().any(char::is_whitespace) {
        format!("\"{path}\"")
    } else {
        path.into_owned()
    };
    let icon = root.join("data/icon.png");
    format!(
        "[Desktop Entry]\nType=Application\nName=Deemo\nComment=Deemo rhythm game\nExec={} %f\nPath={}\nIcon={}\nMimeType=application/x-deemopack;\nTerminal=false\nCategories=Game;\n",
        desktop_exec(exe),
        path,
        icon.display()
    )
}

#[cfg(target_os = "linux")]
const MIME_FILE: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mime-info xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\">\n  <mime-type type=\"application/x-deemopack\">\n    <comment>Deemo pack</comment>\n    <glob pattern=\"*.deemopack\"/>\n  </mime-type>\n</mime-info>\n";

/// Ensure both files exist with the current executable; returns true when at
/// least one file was created or changed. `run_tools` is false in tests.
#[cfg(target_os = "linux")]
fn linux_install(data: &Path, exe: &Path, run_tools: bool) -> Result<bool, String> {
    let mime_dir = data.join("mime/packages");
    let apps_dir = data.join("applications");
    let mime_path = mime_dir.join("deemo-deemopack.xml");
    let desktop_path = apps_dir.join("deemo.desktop");
    let desired_desktop = desktop_file(exe);

    let mime_changed = write_if_changed(&mime_path, MIME_FILE.as_bytes())?;
    let desktop_changed = write_if_changed(&desktop_path, desired_desktop.as_bytes())?;
    let changed = mime_changed || desktop_changed;

    if changed && run_tools {
        // Spawn and ignore: path tools may be missing on a minimal system.
        let _ = std::process::Command::new("update-mime-database")
            .arg(data.join("mime"))
            .status();
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&apps_dir)
            .status();
        // Only when the desktop file is new or its Exec moved.
        if desktop_changed {
            let _ = std::process::Command::new("xdg-mime")
                .args(["default", "deemo.desktop", "application/x-deemopack"])
                .status();
        }
    }
    Ok(changed)
}

/// Create parent dirs, write only when the bytes differ; errors are strings so
/// the caller can log/skip without panicking.
#[cfg(target_os = "linux")]
fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<bool, String> {
    if std::fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(target_os = "windows")]
fn windows_register(exe: &Path) -> Result<String, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn query(key: HKEY) -> Option<String> {
        let mut ty = 0u32;
        let mut len = 0u32;
        if RegQueryValueExW(key, std::ptr::null(), std::ptr::null(), &mut ty, std::ptr::null_mut(), &mut len) != 0 {
            return None;
        }
        if ty != REG_SZ || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if RegQueryValueExW(
            key,
            std::ptr::null(),
            std::ptr::null(),
            &mut ty,
            buf.as_mut_ptr(),
            &mut len,
        ) != 0
        {
            return None;
        }
        let words: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&w| w != 0)
            .collect();
        String::from_utf16(&words).ok()
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn open(root: &str) -> Option<HKEY> {
        let w = wide(root);
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w.as_ptr(), 0, KEY_READ, &mut key) == 0 {
            Some(key)
        } else {
            None
        }
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn set(root: &str, value: &str) -> Result<(), String> {
        let w = wide(root);
        let mut key: HKEY = std::ptr::null_mut();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        ) != 0
        {
            return Err(format!("cannot create {root}"));
        }
        let data: Vec<u8> = value
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(|w| w.to_le_bytes())
            .collect();
        let rc = RegSetValueExW(
            key,
            std::ptr::null(),
            0,
            REG_SZ,
            data.as_ptr(),
            data.len() as u32,
        );
        RegCloseKey(key);
        if rc == 0 { Ok(()) } else { Err(format!("cannot set {root}")) }
    }

    let command = format!("\"{}\" \"%1\"", exe.display());
    let progid = "Software\\Classes\\.deemopack";
    let cmd_key = "Software\\Classes\\DeemoPack\\shell\\open\\command";

    // Quick up-to-date check: ProgID link and exact command.
    let progid_ok = unsafe {
        open(progid).map(|k| {
            let v = query(k);
            RegCloseKey(k);
            v.as_deref() == Some("DeemoPack")
        }).unwrap_or(false)
    };
    let cmd_ok = unsafe {
        open(cmd_key).map(|k| {
            let v = query(k);
            RegCloseKey(k);
            v.as_deref() == Some(command.as_str())
        }).unwrap_or(false)
    };
    if progid_ok && cmd_ok {
        return Ok("assoc: up to date".to_string());
    }

    unsafe {
        set("Software\\Classes\\DeemoPack", "Deemo Pack")?;
        set(
            "Software\\Classes\\DeemoPack\\DefaultIcon",
            &format!("\"{}\",0", exe.display()),
        )?;
        set(cmd_key, &command)?;
        set(progid, "DeemoPack")?;
    }
    Ok(format!("assoc: registered .deemopack -> {}", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_files_are_current_and_idempotent() {
        let dir = std::env::temp_dir().join(format!("deemo-assoc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let exe = Path::new("/opt/Deemo/deemo");
        assert!(linux_install(&dir, exe, false).unwrap(), "first run writes");
        let mime = std::fs::read_to_string(dir.join("mime/packages/deemo-deemopack.xml")).unwrap();
        assert!(mime.contains("application/x-deemopack"));
        assert!(mime.contains("*.deemopack"));
        let desk = std::fs::read_to_string(dir.join("applications/deemo.desktop")).unwrap();
        assert!(desk.contains("Exec=/opt/Deemo/deemo %f"), "{desk}");
        assert!(desk.contains("Path=/opt/Deemo"), "{desk}");
        assert!(desk.contains("MimeType=application/x-deemopack;"));
        assert!(desk.contains("Terminal=false"));
        // second run: exact same bytes, no rewrite
        let before = std::fs::metadata(dir.join("applications/deemo.desktop"))
            .unwrap()
            .modified()
            .unwrap();
        assert!(!linux_install(&dir, exe, false).unwrap(), "already current");
        let after = std::fs::metadata(dir.join("applications/deemo.desktop"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
        // moved executable updates the Exec line
        let moved = Path::new("/opt/Deemo moved/deemo");
        assert!(linux_install(&dir, moved, false).unwrap());
        let desk = std::fs::read_to_string(dir.join("applications/deemo.desktop")).unwrap();
        assert!(desk.contains("Exec=\"/opt/Deemo moved/deemo\" %f"), "{desk}");
        assert!(desk.contains("Path=\"/opt/Deemo moved\""), "{desk}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn desktop_exec_quotes_only_when_needed() {
        assert_eq!(desktop_exec(Path::new("/opt/deemo")), "/opt/deemo");
        assert_eq!(
            desktop_exec(Path::new("/opt/Deemo demo/deemo")),
            "\"/opt/Deemo demo/deemo\""
        );
    }
}
