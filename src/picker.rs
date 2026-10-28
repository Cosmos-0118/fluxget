use crate::{
    resolver::{Candidate, Kind},
    sites,
};
use anyhow::{bail, ensure, Result};
use dialoguer::{theme::ColorfulTheme, Select};
use std::io::{self, IsTerminal};

/// Labels originate in untrusted HTML. Strip terminal controls before rendering.
pub fn plain(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(140)
        .collect()
}

fn score(candidate: &Candidate) -> i32 {
    let text = format!("{} {}", candidate.label, candidate.url).to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    let windows = has(&[
        "windows", "win32", "win64", "-win-", "_win_", "msvc", ".exe", ".msi", ".msix",
    ]);
    let macos = has(&["macos", "darwin", "mac ", "osx", "apple", ".dmg", ".pkg"]);
    let linux = has(&["linux", ".appimage", ".deb", ".rpm", ".snap"]);
    let os_match = match std::env::consts::OS {
        "windows" => windows,
        "macos" => macos,
        "linux" => linux,
        _ => false,
    };
    let mut score = if os_match { 10 } else { 0 };
    let arm = has(&["arm64", "aarch64", "armv8"]);
    let x64 = has(&["x64", "x86_64", "amd64", "win64", "x86-64"]);
    if (std::env::consts::ARCH == "aarch64" && arm) || (std::env::consts::ARCH == "x86_64" && x64) {
        score += 3;
    }
    if (std::env::consts::ARCH == "aarch64" && x64) || (std::env::consts::ARCH == "x86_64" && arm) {
        score -= 3;
    }
    if text.contains("user installer") || text.contains("-user/") || text.contains("-user&") {
        score += 2;
    }
    if text.contains("cli") {
        score -= 2;
    }
    // Checksums, signatures and source archives are rarely what was wanted.
    if has(&[
        ".sha256",
        ".sha512",
        ".sha1",
        ".md5",
        "checksum",
        "sha256sum",
        ".sig",
        ".asc",
        ".minisig",
        ".sbom",
        ".pem",
        ".sigstore",
    ]) {
        score -= 8;
    }
    if has(&["source code", ".src."]) {
        score -= 5;
    }
    score
}

/// What to show beside a candidate: its host, or the tool that will fetch it.
fn origin_tag(candidate: &Candidate) -> String {
    match candidate.kind {
        Kind::Torrent => "torrent".into(),
        Kind::Media => "media".into(),
        Kind::Http => url::Url::parse(&candidate.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default(),
    }
}

/// The entry the interactive picker highlights first.
pub fn default_index(links: &[Candidate]) -> usize {
    links
        .iter()
        .enumerate()
        .max_by_key(|(i, c)| (score(c), std::cmp::Reverse(*i)))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// No person to ask: JSON output, or input or output is not a terminal.
pub fn headless(json: bool) -> bool {
    json || !io::stdin().is_terminal() || !io::stderr().is_terminal()
}

/// Whether `choose` will block on a person, which is the time to get ahead.
pub fn will_prompt(
    links: &[Candidate],
    selection: Option<usize>,
    force_picker: bool,
    json: bool,
) -> bool {
    selection.is_none() && (links.len() > 1 || force_picker) && !headless(json)
}

pub fn choose(
    links: &[Candidate],
    selection: Option<usize>,
    force_picker: bool,
    json: bool,
) -> Result<Option<usize>> {
    ensure!(
        !links.is_empty(),
        "No downloadable files found. Try a direct file URL or --direct to save the page itself"
    );
    if let Some(index) = selection {
        ensure!(
            index > 0 && index <= links.len(),
            "--select must be between 1 and {}",
            links.len()
        );
        return Ok(Some(index - 1));
    }
    if links.len() == 1 && !force_picker {
        return Ok(Some(0));
    }
    if headless(json) {
        let choices = links
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let tag = origin_tag(c);
                format!(
                    "  {}. {} [{tag}]\n     {}",
                    i + 1,
                    plain(&c.label),
                    sites::short(&c.url)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        bail!("Found {} downloads. Use --select N to choose one without a terminal, or 'fluxget resolve URL' for JSON.\n{choices}", links.len());
    }
    let default = default_index(links);
    let width = dialoguer::console::Term::stderr().size().1.max(40) as usize;
    let items = links
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let text = format!("{:>3}. {}  [{}]", i + 1, plain(&c.label), origin_tag(c));
            dialoguer::console::truncate_str(&text, width.saturating_sub(6), "...").into_owned()
        })
        .collect::<Vec<_>>();
    eprintln!(
        "Found {} downloads. Use Up/Down, Enter to download, Esc to cancel.",
        links.len()
    );
    let theme = ColorfulTheme::default();
    let choice = Select::with_theme(&theme)
        .with_prompt("Choose a download")
        .items(&items)
        .default(default)
        .max_length(12)
        .interact_opt()?;
    if let Some(index) = choice {
        eprintln!(
            "Selected: {}\n{}",
            plain(&links[index].label),
            sites::short(&links[index].url)
        );
    }
    Ok(choice)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hostile_html_cannot_emit_terminal_controls() {
        assert!(!plain("file\x1b[2J\u{202e}evil\nname").contains(['\x1b', '\u{202e}', '\n']));
    }
    #[test]
    fn default_choice_prefers_the_installer_over_checksums_and_source() {
        let installer = Candidate::new(
            "https://h.example/tool-1.0-x86_64-pc-windows-msvc.zip",
            "tool windows x64",
        );
        let checksum = Candidate::new(
            "https://h.example/tool-1.0-x86_64-pc-windows-msvc.zip.sha256",
            "tool windows x64 checksum",
        );
        let source = Candidate::new("https://h.example/v1.0.zip", "Source code (zip)");
        let linux = Candidate::new("https://h.example/tool-linux.tar.gz", "tool linux");
        assert!(score(&installer) > score(&checksum));
        assert!(score(&installer) > score(&source));
        if std::env::consts::OS == "windows" {
            assert!(score(&installer) > score(&linux));
        }
        assert_eq!(
            origin_tag(&Candidate::new(
                "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a",
                "m"
            )),
            "torrent"
        );
    }
    #[test]
    fn headless_selection_is_explicit_and_bounded() {
        let links = vec![
            Candidate::new("https://example.com/a.zip", "A"),
            Candidate::new("https://example.com/b.zip", "B"),
        ];
        assert_eq!(choose(&links, Some(2), false, true).unwrap(), Some(1));
        assert!(choose(&links, Some(0), false, true).is_err());
        assert!(choose(&links, Some(3), false, true).is_err());
        assert!(choose(&links, None, false, true).is_err());
    }
}
