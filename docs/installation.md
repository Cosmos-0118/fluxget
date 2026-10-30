# Installation and updates

[Back to README](../README.md) · [Usage](usage.md) · [Installation](installation.md) · [Integrity](integrity.md) · [Development](development.md)

No Rust, Python, admin account, or compiler is required for prebuilt releases.
The installer automatically selects your OS and CPU, downloads the latest
release, checks its SHA-256, verifies the executable, and installs it for your
user account. Open a new terminal after installation and run `fluxget --help`.

**Windows — paste into PowerShell:**

```powershell
irm https://raw.githubusercontent.com/Cosmos-0118/fluxget/main/install.ps1 | iex
```

**macOS or Linux — paste into Terminal:**

```sh
curl -fsSL https://raw.githubusercontent.com/Cosmos-0118/fluxget/main/install.sh | bash
```

In a terminal the installer shows a live step list with a flowing wave and a
progress bar (size, speed). Piped or CI output gets plain `[n/total]` lines; set
`FLUXGET_PLAIN=1` to force that, `FLUXGET_ASCII=1` for ASCII-only glyphs, or
`NO_COLOR=1` to drop colour.

Prefer clicking? Download the matching ZIP from
[Releases](https://github.com/Cosmos-0118/fluxget/releases/latest), extract it,
then double-click **Install.cmd** on Windows or **Install.command** on macOS.
If macOS blocks the unsigned installer, use the Terminal command above.
Linux packages also include `install.sh`. Packages contain license notices and
both an archive checksum and an internal executable checksum. Checksums detect
corruption; they rely on the trusted GitHub release as the source.

**Already installed? Run the same command again to update.** An identical binary
is left in place; a changed binary replaces the existing installation only after
verification. A failed download, checksum or version check leaves the old binary
intact. PATH entries are not duplicated. Downloads and resume checkpoints are
preserved; updating does not uninstall your data or create another installation.
An explicitly selected older release can also replace the installed version.

Default locations: `%LOCALAPPDATA%\Programs\fluxget\bin` on Windows and
`~/.local/bin` on macOS/Linux. Windows updates the user PATH; Unix updates shell
profiles for bash/zsh. Restart your terminal after installation or updates.
Installation from an extracted archive uses that archive's version; the online
command always fetches the latest published release.

For a custom directory or a pinned version:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/Cosmos-0118/fluxget/main/install.ps1))) -Version v0.1.0 -InstallDir "$env:LOCALAPPDATA\Programs\fluxget"
```

```sh
curl -fsSL https://raw.githubusercontent.com/Cosmos-0118/fluxget/main/install.sh | bash -s -- --version v0.1.0 --prefix "$HOME/apps/fluxget"
```

Extracted-package options: `powershell -ExecutionPolicy Bypass -File install.ps1
-InstallDir C:\Tools\fluxget -NoModifyPath` or `bash install.sh --prefix
"$HOME/apps/fluxget" --no-modify-path`. The no-modify option leaves PATH/profile
settings unchanged. Running an installer from a source checkout uses its local
binary, or builds it if the prerequisites below are available.

