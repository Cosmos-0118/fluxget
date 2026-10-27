//! Small adapters for public landing pages with platform-specific JS buttons.
//! VS Code platform identifiers follow Microsoft's published download FAQ and
//! the current public /dist/index.js mapping; no page scripts are executed.
use url::Url;

pub fn vscode(url: &Url) -> Option<(Url, &'static str)> {
    if url.host_str()? != "code.visualstudio.com" {
        return None;
    }
    let values: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    let key = values.get("dv")?;
    let (platform, label) = match key.as_str() {
        "win" | "win64user" => ("win32-x64-user", "Windows - User Installer (x64)"),
        "win64" => ("win32-x64", "Windows - System Installer (x64)"),
        "winzip" => ("win32-x64-archive", "Windows - ZIP archive (x64)"),
        "wincli" => ("cli-win32-x64", "Windows - CLI (x64)"),
        "win32arm64user" => ("win32-arm64-user", "Windows - User Installer (ARM64)"),
        "win32arm64setup" => ("win32-arm64", "Windows - System Installer (ARM64)"),
        "win32arm64zip" => ("win32-arm64-archive", "Windows - ZIP archive (ARM64)"),
        "win32arm64cli" => ("cli-win32-arm64", "Windows - CLI (ARM64)"),
        "osx" => ("darwin-universal-dmg", "macOS - DMG (Universal)"),
        "darwinx64" => ("darwin-x64-dmg", "macOS - DMG (Intel)"),
        "darwinarm64" => ("darwin-arm64-dmg", "macOS - DMG (Apple Silicon)"),
        "darwinx64cli" => ("cli-darwin-x64", "macOS - CLI (Intel)"),
        "darwinarm64cli" => ("cli-darwin-arm64", "macOS - CLI (Apple Silicon)"),
        "linux" | "linux64" => ("linux-x64", "Linux - tar.gz (x64)"),
        "linuxarm64" => ("linux-arm64", "Linux - tar.gz (ARM64)"),
        "linuxarmhf" => ("linux-armhf", "Linux - tar.gz (ARM32)"),
        "linux64_deb" => ("linux-deb-x64", "Linux - Debian/Ubuntu .deb (x64)"),
        "linuxarm64_deb" => ("linux-deb-arm64", "Linux - Debian/Ubuntu .deb (ARM64)"),
        "linuxarmhf_deb" => ("linux-deb-armhf", "Linux - Debian/Ubuntu .deb (ARM32)"),
        "linux64_rpm" => ("linux-rpm-x64", "Linux - Fedora/Red Hat .rpm (x64)"),
        "linuxarm64_rpm" => ("linux-rpm-arm64", "Linux - Fedora/Red Hat .rpm (ARM64)"),
        "linuxarmhf_rpm" => ("linux-rpm-armhf", "Linux - Fedora/Red Hat .rpm (ARM32)"),
        "linux64cli" => ("cli-alpine-x64", "Linux - CLI (x64)"),
        "linuxarm64cli" => ("cli-alpine-arm64", "Linux - CLI (ARM64)"),
        "linuxarmhfcli" => ("cli-linux-armhf", "Linux - CLI (ARM32)"),
        _ => return None,
    };
    let channel = if values.get("build").is_some_and(|v| v == "insider") {
        "insider"
    } else {
        "stable"
    };
    Some((
        Url::parse(&format!(
            "https://update.code.visualstudio.com/latest/{platform}/{channel}"
        ))
        .ok()?,
        label,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_official_buttons_and_preserves_channel() {
        let (url, label) = vscode(
            &Url::parse("https://code.visualstudio.com/thank-you?dv=win64user&build=stable")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://update.code.visualstudio.com/latest/win32-x64-user/stable"
        );
        assert!(label.contains("User Installer"));
        let (url, _) = vscode(
            &Url::parse("https://code.visualstudio.com/thank-you?dv=darwinarm64&build=insider")
                .unwrap(),
        )
        .unwrap();
        assert!(url.as_str().ends_with("/darwin-arm64-dmg/insider"));
        assert!(
            vscode(&Url::parse("https://other.example/thank-you?dv=win64user").unwrap()).is_none()
        );
        assert!(
            vscode(&Url::parse("https://code.visualstudio.com/thank-you?dv=unknown").unwrap())
                .is_none()
        );
    }
}
