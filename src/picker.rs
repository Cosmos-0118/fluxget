use crate::resolver::Candidate;
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
    let os_match = match std::env::consts::OS {
        "windows" => text.contains("windows") || text.contains("win32"),
        "macos" => text.contains("macos") || text.contains("darwin") || text.contains("mac "),
        "linux" => text.contains("linux"),
        _ => false,
    };
    let mut score = if os_match { 10 } else { 0 };
    let arm = text.contains("arm64") || text.contains("aarch64");
    let x64 = text.contains("x64") || text.contains("x86_64") || text.contains("amd64");
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
    score
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
    if json || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        let choices = links
            .iter()
            .enumerate()
            .map(|(i, c)| format!("  {}. {}\n     {}", i + 1, plain(&c.label), c.url))
            .collect::<Vec<_>>()
            .join("\n");
        bail!("Found {} downloads. Use --select N to choose one without a terminal, or 'fluxget resolve URL' for JSON.\n{choices}", links.len());
    }
    let default = links
        .iter()
        .enumerate()
        .max_by_key(|(i, c)| (score(c), std::cmp::Reverse(*i)))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let width = dialoguer::console::Term::stderr().size().1.max(40) as usize;
    let items = links
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let host = url::Url::parse(&c.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default();
            let text = format!("{:>3}. {}  [{}]", i + 1, plain(&c.label), host);
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
            links[index].url
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
    fn headless_selection_is_explicit_and_bounded() {
        let links = vec![
            Candidate {
                url: "https://example.com/a.zip".into(),
                label: "A".into(),
            },
            Candidate {
                url: "https://example.com/b.zip".into(),
                label: "B".into(),
            },
        ];
        assert_eq!(choose(&links, Some(2), false, true).unwrap(), Some(1));
        assert!(choose(&links, Some(0), false, true).is_err());
        assert!(choose(&links, Some(3), false, true).is_err());
        assert!(choose(&links, None, false, true).is_err());
    }
}
