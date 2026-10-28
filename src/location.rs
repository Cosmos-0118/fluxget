//! Where a download is saved: the Downloads folder unless the person picks another.

use crate::ui;
use anyhow::{Context, Result};
use dialoguer::{Input, Select};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Overrides the default folder, for scripts and tests.
pub const ENV: &str = "FLUXGET_DOWNLOAD_DIR";

/// The folder used when no `-o` is given.
pub fn downloads_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `dir/filename`, or `dir/name (1).ext` and so on when that name is taken,
/// the way browsers avoid overwriting an earlier download.
pub fn free_name(dir: &Path, filename: &str) -> PathBuf {
    let first = dir.join(filename);
    if !first.exists() {
        return first;
    }
    let lower = filename.to_ascii_lowercase();
    let split = [".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst"]
        .iter()
        .find(|ext| lower.ends_with(**ext))
        .map(|ext| filename.len() - ext.len())
        .or_else(|| filename.rfind('.').filter(|&at| at > 0))
        .unwrap_or(filename.len());
    let (stem, ext) = filename.split_at(split);
    (1..10_000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// Asks where to save. `None` means the person cancelled.
pub fn ask() -> Result<Option<PathBuf>> {
    let default = downloads_dir();
    let name = if std::env::var_os(ENV).is_some_and(|v| !v.is_empty()) {
        "Default folder"
    } else {
        "Downloads"
    };
    let items = [
        format!("{name}  ({})", default.display()),
        "Pick a folder...".to_string(),
    ];
    let theme = ui::theme();
    eprintln!();
    loop {
        let choice = Select::with_theme(&theme)
            .with_prompt("Save to")
            .items(&items)
            .default(0)
            .interact_opt()?;
        match choice {
            None => return Ok(None),
            Some(0) => return Ok(Some(default)),
            Some(_) => match pick_folder(&default)? {
                Picked::Dir(dir) => return Ok(Some(dir)),
                // A closed picker returns to the two choices.
                Picked::Cancelled => continue,
                Picked::Unavailable => return type_folder(&default).map(Some),
            },
        }
    }
}

enum Picked {
    Dir(PathBuf),
    Cancelled,
    Unavailable,
}

fn type_folder(default: &Path) -> Result<PathBuf> {
    let typed: String = Input::with_theme(&ui::theme())
        .with_prompt("Folder path")
        .default(default.display().to_string())
        .interact_text()?;
    let typed = typed.trim().trim_matches('"');
    Ok(match typed.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => dirs::home_dir()
            .context("Cannot find your home folder")?
            .join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(typed),
    })
}

/// Runs the system's own folder picker and reads the chosen path from stdout.
/// An empty answer is a cancelled dialog; a tool that cannot start is unavailable.
fn run_picker(mut command: Command) -> Picked {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(output) = command.output() else {
        return Picked::Unavailable;
    };
    let chosen = String::from_utf8_lossy(&output.stdout);
    let chosen = chosen.trim_end_matches(['\r', '\n']);
    let chosen = if chosen.len() > 1 {
        chosen.trim_end_matches(['/', '\\'])
    } else {
        chosen
    };
    if output.status.success() && !chosen.is_empty() {
        Picked::Dir(PathBuf::from(chosen))
    } else {
        Picked::Cancelled
    }
}

#[cfg(windows)]
fn pick_folder(start: &Path) -> Result<Picked> {
    use base64::Engine;
    eprintln!("Opening the folder picker...");
    let utf16: Vec<u8> = PICKER_SCRIPT
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let mut command =
        Command::new(Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &base64::engine::general_purpose::STANDARD.encode(utf16),
        ])
        .env("FLUXGET_PICK_START", start)
        .env("FLUXGET_PICK_TITLE", "Choose where to save downloads");
    Ok(run_picker(command))
}

/// The Windows folder picker is the shell's IFileOpenDialog in folder mode, the same
/// dialog other modern apps use, reached through a few lines of C# that PowerShell compiles.
#[cfg(windows)]
const PICKER_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class FluxPick {
    [ComImport, Guid("DC1C5A9C-E88A-4DDE-A5A1-60F82A20AEF7")]
    class FileOpenDialog { }
    [ComImport, Guid("42F85136-DB7E-439C-85F1-E4075D135FC8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IFileDialog {
        [PreserveSig] int Show(IntPtr parent);
        void SetFileTypes(uint count, IntPtr types);
        void SetFileTypeIndex(uint index);
        void GetFileTypeIndex(out uint index);
        void Advise(IntPtr events, out uint cookie);
        void Unadvise(uint cookie);
        void SetOptions(uint options);
        void GetOptions(out uint options);
        void SetDefaultFolder(IShellItem item);
        void SetFolder(IShellItem item);
        void GetFolder(out IShellItem item);
        void GetCurrentSelection(out IShellItem item);
        void SetFileName([MarshalAs(UnmanagedType.LPWStr)] string name);
        void GetFileName([MarshalAs(UnmanagedType.LPWStr)] out string name);
        void SetTitle([MarshalAs(UnmanagedType.LPWStr)] string title);
        void SetOkButtonLabel([MarshalAs(UnmanagedType.LPWStr)] string text);
        void SetFileNameLabel([MarshalAs(UnmanagedType.LPWStr)] string text);
        void GetResult(out IShellItem item);
        void AddPlace(IShellItem item, int placement);
        void SetDefaultExtension([MarshalAs(UnmanagedType.LPWStr)] string extension);
        void Close(int hr);
        void SetClientGuid(ref Guid guid);
        void ClearClientData();
        void SetFilter(IntPtr filter);
    }
    [ComImport, Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellItem {
        void BindToHandler(IntPtr context, ref Guid handler, ref Guid riid, out IntPtr result);
        void GetParent(out IShellItem item);
        void GetDisplayName(uint form, [MarshalAs(UnmanagedType.LPWStr)] out string name);
        void GetAttributes(uint mask, out uint attributes);
        void Compare(IShellItem other, uint hint, out int order);
    }
    [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = false)]
    static extern void SHCreateItemFromParsingName(string path, IntPtr context, ref Guid riid, out IShellItem item);
    [DllImport("user32.dll")]
    static extern IntPtr GetForegroundWindow();
    public static string Pick(string title, string start) {
        IFileDialog dialog = (IFileDialog)new FileOpenDialog();
        uint options;
        dialog.GetOptions(out options);
        dialog.SetOptions(options | 0x20 | 0x40 | 0x800);
        dialog.SetTitle(title);
        dialog.SetOkButtonLabel("Select folder");
        if (!String.IsNullOrEmpty(start) && System.IO.Directory.Exists(start)) {
            Guid iid = new Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE");
            IShellItem folder;
            SHCreateItemFromParsingName(start, IntPtr.Zero, ref iid, out folder);
            dialog.SetFolder(folder);
        }
        if (dialog.Show(GetForegroundWindow()) != 0) return null;
        IShellItem result;
        dialog.GetResult(out result);
        string path;
        result.GetDisplayName(0x80058000, out path);
        return path;
    }
}
'@
$picked = [FluxPick]::Pick($env:FLUXGET_PICK_TITLE, $env:FLUXGET_PICK_START)
if ($picked) { [Console]::Out.Write($picked) }
"#;

#[cfg(target_os = "macos")]
fn pick_folder(start: &Path) -> Result<Picked> {
    eprintln!("Opening the folder picker...");
    let mut command = Command::new("osascript");
    command.args([
        "-e",
        "on run argv",
        "-e",
        "if (count of argv) > 1 then",
        "-e",
        "POSIX path of (choose folder with prompt (item 1 of argv) default location (POSIX file (item 2 of argv)))",
        "-e",
        "else",
        "-e",
        "POSIX path of (choose folder with prompt (item 1 of argv))",
        "-e",
        "end if",
        "-e",
        "end run",
        "--",
        "Choose where to save downloads",
    ]);
    if start.is_dir() {
        command.arg(start);
    }
    Ok(run_picker(command))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn pick_folder(start: &Path) -> Result<Picked> {
    let has_display =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if !has_display {
        return Ok(Picked::Unavailable);
    }
    eprintln!("Opening the folder picker...");
    let mut start_dir = start.as_os_str().to_owned();
    start_dir.push("/");
    let mut zenity = Command::new("zenity");
    zenity
        .args([
            "--file-selection",
            "--directory",
            "--title=Choose where to save downloads",
        ])
        .arg({
            let mut arg = std::ffi::OsString::from("--filename=");
            arg.push(&start_dir);
            arg
        });
    match run_picker(zenity) {
        Picked::Unavailable => {
            let mut kdialog = Command::new("kdialog");
            kdialog
                .arg("--getexistingdirectory")
                .arg(&start_dir)
                .args(["--title", "Choose where to save downloads"]);
            Ok(run_picker(kdialog))
        }
        found => Ok(found),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taken_names_get_a_counter_like_a_browser() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(free_name(dir.path(), "a.zip"), dir.path().join("a.zip"));
        std::fs::write(dir.path().join("a.zip"), b"x").unwrap();
        assert_eq!(free_name(dir.path(), "a.zip"), dir.path().join("a (1).zip"));
        std::fs::write(dir.path().join("a (1).zip"), b"x").unwrap();
        assert_eq!(free_name(dir.path(), "a.zip"), dir.path().join("a (2).zip"));
        std::fs::write(dir.path().join("b.tar.gz"), b"x").unwrap();
        assert_eq!(
            free_name(dir.path(), "b.tar.gz"),
            dir.path().join("b (1).tar.gz")
        );
        std::fs::write(dir.path().join("README"), b"x").unwrap();
        assert_eq!(
            free_name(dir.path(), "README"),
            dir.path().join("README (1)")
        );
        std::fs::write(dir.path().join(".hidden"), b"x").unwrap();
        assert_eq!(
            free_name(dir.path(), ".hidden"),
            dir.path().join(".hidden (1)")
        );
    }

    #[test]
    fn default_folder_is_downloads_and_can_be_overridden() {
        std::env::remove_var(ENV);
        let default = downloads_dir();
        assert!(
            default.ends_with("Downloads") || default.as_os_str() == ".",
            "{default:?}"
        );
        std::env::set_var(ENV, "custom-place");
        assert_eq!(downloads_dir(), PathBuf::from("custom-place"));
        std::env::remove_var(ENV);
    }
}
