//! What popular download sources look like as URLs. Everything here is pure:
//! no network access and no page scripts. Network-assisted expansion (release
//! APIs, file listings) lives in `forges.rs`.
//!
//! VS Code platform identifiers follow Microsoft's published download FAQ and
//! the current public /dist/index.js mapping.
use crate::{
    forges::{Forge, Scope},
    resolver::Candidate,
};
use base64::Engine as _;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use url::Url;

pub enum Route {
    /// Nothing site-specific: fetch the URL and read what comes back.
    Generic,
    /// A share link with a known direct-download form.
    Rewrite(Candidate),
    /// Fixed choices derived from the URL alone.
    Choices(Vec<Candidate>),
    /// A media page that yt-dlp understands.
    Media,
    /// A site that cannot be downloaded from natively, with the reason.
    Blocked(&'static str),
    /// A site with an API listing its downloadable files.
    Api(Forge),
}

const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encode one path segment.
pub fn enc(segment: &str) -> String {
    utf8_percent_encode(segment, SEGMENT).to_string()
}

/// `base` plus path segments; a segment containing `/` becomes several.
pub fn join(base: &str, segments: &[&str]) -> String {
    let mut out = base.trim_end_matches('/').to_owned();
    for part in segments.iter().flat_map(|s| s.split('/')) {
        out.push('/');
        out.push_str(&enc(part));
    }
    out
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub struct GitHub {
    pub web: Url,
    pub api: String,
}

/// github.com, or a GitHub Enterprise Server named by `FLUXGET_GITHUB_URL`
/// (its API lives under `/api/v3`).
pub fn github() -> GitHub {
    let configured = std::env::var("FLUXGET_GITHUB_URL")
        .ok()
        .and_then(|v| Url::parse(v.trim()).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some());
    match configured {
        Some(web) => {
            let api = format!("{}/api/v3", web.as_str().trim_end_matches('/'));
            GitHub { web, api }
        }
        None => GitHub {
            web: Url::parse("https://github.com").unwrap(),
            api: "https://api.github.com".into(),
        },
    }
}

pub fn route(url: &Url) -> Route {
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return Route::Generic;
    };
    let bare = host.strip_prefix("www.").unwrap_or(&host);
    let segments: Vec<String> = url
        .path_segments()
        .map(|parts| {
            parts
                .filter(|p| !p.is_empty())
                .map(|p| percent_decode_str(p).decode_utf8_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let s: Vec<&str> = segments.iter().map(String::as_str).collect();
    if let Some(route) = github_route(url, bare, &s) {
        return route;
    }
    if bare == "code.visualstudio.com" {
        match s.as_slice() {
            [] | ["download"] | ["Download"] => return Route::Choices(vscode_all("stable")),
            ["insiders"] => return Route::Choices(vscode_all("insider")),
            _ => {}
        }
    }
    let known = match bare {
        "dropbox.com" => Some(dropbox(url, &s)),
        "drive.google.com" => Some(google_drive(url, &s)),
        "docs.google.com" => Some(google_docs(&s)),
        "1drv.ms" | "onedrive.live.com" => Some(onedrive(url, &s)),
        "mega.nz" | "mega.io" => Some(Route::Blocked(
            "MEGA files are end-to-end encrypted and cannot be fetched over plain HTTP. Use MEGAcmd or megatools",
        )),
        "pixeldrain.com" => Some(pixeldrain(&s)),
        "huggingface.co" | "hf.co" => Some(hugging_face(&s)),
        "archive.org" => Some(archive_org(&s)),
        "sourceforge.net" => Some(sourceforge(&s)),
        _ => None,
    };
    if let Some(route) = known {
        return route;
    }
    if let Some(route) = gitlab_route(url, bare, &s) {
        return route;
    }
    if media_page(url, bare, &s) {
        return Route::Media;
    }
    gitea_route(url, bare, &s).unwrap_or(Route::Generic)
}

fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

fn github_route(url: &Url, host: &str, s: &[&str]) -> Option<Route> {
    const RESERVED: &[&str] = &[
        "orgs",
        "settings",
        "marketplace",
        "topics",
        "features",
        "about",
        "pricing",
        "explore",
        "sponsors",
        "login",
        "join",
        "notifications",
        "search",
        "collections",
        "trending",
        "enterprise",
        "customer-stories",
        "security",
        "readme",
        "apps",
        "users",
        "new",
        "codespaces",
        "issues",
    ];
    let gh = github();
    let same_site = if gh.web.host_str() == Some("github.com") {
        host == "github.com"
    } else {
        url.host_str() == gh.web.host_str()
            && url.port_or_known_default() == gh.web.port_or_known_default()
    };
    if !same_site || s.len() < 2 || RESERVED.contains(&s[0]) {
        return None;
    }
    let (owner, repo) = (s[0].to_owned(), s[1].trim_end_matches(".git").to_owned());
    let web = gh.web.as_str().trim_end_matches('/').to_owned();
    let forge = |scope| {
        Some(Route::Api(Forge::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
            scope,
        }))
    };
    match s.get(2).copied() {
        None => forge(Scope::Repo),
        Some("releases") => match s.get(3).copied() {
            None => forge(Scope::List),
            Some("latest") => forge(Scope::Latest),
            Some("tag" | "expanded_assets") if s.len() > 4 => forge(Scope::Tag(s[4..].join("/"))),
            _ => None,
        },
        Some("blob") if s.len() >= 5 => Some(Route::Rewrite(Candidate::new(
            join(&web, &[&owner, &repo, "raw", &s[3..].join("/")]),
            format!("Raw file {}", s[s.len() - 1]),
        ))),
        Some("tree") if s.len() >= 4 => {
            let reference = s[3..].join("/");
            let archive = |ext: &str| join(&web, &[&owner, &repo, "archive", &reference]) + ext;
            Some(Route::Choices(vec![
                Candidate::new(archive(".zip"), format!("Source code (zip) - {reference}")),
                Candidate::new(
                    archive(".tar.gz"),
                    format!("Source code (tar.gz) - {reference}"),
                ),
            ]))
        }
        _ => None,
    }
}

fn gitlab_route(url: &Url, host: &str, s: &[&str]) -> Option<Route> {
    let origin = origin(url);
    let forge = |project: &[&str], scope| {
        Some(Route::Api(Forge::GitLab {
            origin: origin.clone(),
            project: project.join("/"),
            scope,
        }))
    };
    if let Some(dash) = s.iter().position(|p| *p == "-") {
        if dash == 0 || s.get(dash + 1) != Some(&"releases") {
            return None;
        }
        let project = &s[..dash];
        return match &s[dash + 2..] {
            [] => forge(project, Scope::List),
            ["permalink", "latest"] => forge(project, Scope::Latest),
            [tag] if !matches!(*tag, "new" | "edit") => {
                forge(project, Scope::Tag((*tag).to_owned()))
            }
            _ => None,
        };
    }
    (host == "gitlab.com" && s.len() == 2).then(|| forge(s, Scope::Repo))?
}

fn gitea_route(url: &Url, host: &str, s: &[&str]) -> Option<Route> {
    let forge = |scope| {
        Some(Route::Api(Forge::Gitea {
            origin: origin(url),
            owner: s[0].to_owned(),
            repo: s[1].to_owned(),
            scope,
        }))
    };
    match s {
        [_, _] if matches!(host, "codeberg.org" | "gitea.com") => forge(Scope::Repo),
        [_, _, "releases"] => forge(Scope::List),
        [_, _, "releases", "latest"] => forge(Scope::Latest),
        [_, _, "releases", "tag", tag @ ..] if !tag.is_empty() => forge(Scope::Tag(tag.join("/"))),
        _ => None,
    }
}

fn dropbox(url: &Url, s: &[&str]) -> Route {
    if !matches!(s.first(), Some(&("s" | "scl" | "sh"))) {
        return Route::Generic;
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    if pairs.iter().any(|(k, v)| k == "dl" && v == "1") {
        return Route::Generic;
    }
    let mut direct = url.clone();
    direct.set_fragment(None);
    direct
        .query_pairs_mut()
        .clear()
        .extend_pairs(pairs.iter().filter(|(k, _)| k != "dl" && k != "raw"))
        .append_pair("dl", "1");
    Route::Rewrite(Candidate::new(direct.as_str(), "Dropbox shared file"))
}

fn google_drive(url: &Url, s: &[&str]) -> Route {
    if s.contains(&"folders") {
        return Route::Blocked(
            "Google Drive folders cannot be downloaded without the Drive API. Open a single file's share link instead",
        );
    }
    let from_path = s
        .windows(3)
        .find(|w| w[0] == "file" && w[1] == "d")
        .map(|w| w[2].to_owned());
    let from_query = matches!(s.first(), Some(&("open" | "uc")))
        .then(|| {
            url.query_pairs()
                .find(|(k, _)| k == "id")
                .map(|(_, v)| v.into_owned())
        })
        .flatten();
    let Some(id) = from_path.or(from_query) else {
        return Route::Generic;
    };
    let mut direct = Url::parse("https://drive.usercontent.google.com/download").unwrap();
    {
        let mut query = direct.query_pairs_mut();
        query
            .append_pair("id", &id)
            .append_pair("export", "download")
            .append_pair("confirm", "t");
        if let Some((_, key)) = url.query_pairs().find(|(k, _)| k == "resourcekey") {
            query.append_pair("resourcekey", &key);
        }
    }
    Route::Rewrite(Candidate::new(direct.as_str(), "Google Drive file"))
}

fn google_docs(s: &[&str]) -> Route {
    let [kind, "d", id, ..] = s else {
        return Route::Generic;
    };
    let formats: &[(&str, &str)] = match *kind {
        "document" => &[
            ("docx", "Word document"),
            ("pdf", "PDF"),
            ("txt", "Plain text"),
        ],
        "spreadsheets" => &[
            ("xlsx", "Excel workbook"),
            ("pdf", "PDF"),
            ("csv", "CSV (first sheet)"),
        ],
        "presentation" => &[("pptx", "PowerPoint"), ("pdf", "PDF")],
        _ => return Route::Generic,
    };
    let noun = match *kind {
        "document" => "document",
        "spreadsheets" => "spreadsheet",
        _ => "presentation",
    };
    Route::Choices(
        formats
            .iter()
            .map(|(ext, name)| {
                Candidate::new(
                    format!(
                        "https://docs.google.com/{kind}/d/{}/export?format={ext}",
                        enc(id)
                    ),
                    format!("Google {noun} as {name}"),
                )
            })
            .collect(),
    )
}

fn onedrive(url: &Url, s: &[&str]) -> Route {
    if s.first() == Some(&"download") {
        return Route::Generic;
    }
    // Public single-file shares work with the documented shares API: "u!" plus
    // the URL-safe base64 of the sharing link.
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url.as_str());
    Route::Rewrite(Candidate::new(
        format!("https://api.onedrive.com/v1.0/shares/u!{token}/root/content"),
        "OneDrive shared file",
    ))
}

fn pixeldrain(s: &[&str]) -> Route {
    match s {
        ["u", id] => Route::Rewrite(Candidate::new(
            format!("https://pixeldrain.com/api/file/{}?download", enc(id)),
            "Pixeldrain file",
        )),
        _ => Route::Generic,
    }
}

fn hugging_face(s: &[&str]) -> Route {
    const RESERVED: &[&str] = &[
        "docs",
        "blog",
        "settings",
        "papers",
        "collections",
        "join",
        "login",
        "pricing",
        "enterprise",
        "api",
        "tasks",
        "organizations",
        "spaces",
        "chat",
    ];
    let (dataset, rest) = match s {
        ["datasets", rest @ ..] => (true, rest),
        _ => (false, s),
    };
    if rest.is_empty() || (!dataset && (rest.len() < 2 || RESERVED.contains(&rest[0]))) {
        return Route::Generic;
    }
    let repo_parts = if dataset && rest.len() == 1 {
        1
    } else {
        2.min(rest.len())
    };
    let repo = rest[..repo_parts].join("/");
    let tail = &rest[repo_parts..];
    let prefix = "https://huggingface.co";
    let base = if dataset {
        join(prefix, &["datasets", &repo])
    } else {
        join(prefix, &[&repo])
    };
    match tail {
        [] => Route::Api(Forge::HuggingFace {
            dataset,
            repo,
            rev: "main".into(),
            path: String::new(),
        }),
        ["tree", rev, path @ ..] => Route::Api(Forge::HuggingFace {
            dataset,
            repo,
            rev: (*rev).to_owned(),
            path: path.join("/"),
        }),
        ["blob", rev, path @ ..] if !path.is_empty() => Route::Rewrite(Candidate::new(
            format!(
                "{}?download=true",
                join(&base, &["resolve", rev, &path.join("/")])
            ),
            format!("File {}", path[path.len() - 1]),
        )),
        _ => Route::Generic,
    }
}

fn archive_org(s: &[&str]) -> Route {
    match s {
        ["details", id] | ["details", id, ..] => Route::Api(Forge::ArchiveOrg {
            id: (*id).to_owned(),
        }),
        _ => Route::Generic,
    }
}

fn sourceforge(s: &[&str]) -> Route {
    match s {
        ["projects", name] | ["projects", name, "files"] => Route::Api(Forge::SourceForge {
            project: (*name).to_owned(),
        }),
        _ => Route::Generic,
    }
}

/// Pages whose content is a video or audio stream that yt-dlp can fetch.
fn media_page(url: &Url, host: &str, s: &[&str]) -> bool {
    let first = s.first().copied().unwrap_or("");
    let has_query = |key: &str| url.query_pairs().any(|(k, _)| k == key);
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    match host {
        "youtube.com" | "m.youtube.com" | "music.youtube.com" => {
            (first == "watch" && has_query("v"))
                || matches!(first, "shorts" | "live" | "embed" | "playlist")
        }
        "youtu.be" | "vm.tiktok.com" | "fb.watch" | "dai.ly" | "b23.tv" | "v.redd.it"
        | "clips.twitch.tv" | "streamable.com" => !s.is_empty(),
        "vimeo.com" | "player.vimeo.com" => numeric(first) || (first == "video" && s.len() > 1),
        "twitter.com" | "mobile.twitter.com" | "x.com" => s.len() >= 3 && s[1] == "status",
        "instagram.com" => matches!(first, "p" | "reel" | "reels" | "tv"),
        "tiktok.com" => s.contains(&"video"),
        "facebook.com" | "m.facebook.com" => {
            matches!(first, "watch" | "reel") || s.contains(&"videos")
        }
        "reddit.com" | "old.reddit.com" => s.len() >= 4 && s[0] == "r" && s[2] == "comments",
        "twitch.tv" | "m.twitch.tv" => first == "videos" || s.get(1) == Some(&"clip"),
        "dailymotion.com" | "bilibili.com" => first == "video",
        "nicovideo.jp" => first == "watch",
        "soundcloud.com" => {
            s.len() >= 2
                && !matches!(
                    first,
                    "discover"
                        | "you"
                        | "search"
                        | "stream"
                        | "upload"
                        | "pages"
                        | "charts"
                        | "settings"
                        | "jobs"
                )
        }
        "rumble.com" => first.starts_with('v'),
        "odysee.com" => first.starts_with('@'),
        "mixcloud.com" => s.len() >= 2,
        "ted.com" => first == "talks",
        h => h.ends_with(".bandcamp.com") && matches!(first, "track" | "album"),
    }
}

/// The info-hash of a magnet link, lower-cased, if it names a torrent.
pub fn magnet_hash(uri: &str) -> Option<String> {
    let uri = uri.trim();
    let query = uri
        .get(..8)
        .filter(|p| p.eq_ignore_ascii_case("magnet:?"))?;
    let query = &uri[query.len()..];
    url::form_urlencoded::parse(query.as_bytes()).find_map(|(key, value)| {
        if !key.eq_ignore_ascii_case("xt") {
            return None;
        }
        let value = value.to_ascii_lowercase();
        let hash = value.strip_prefix("urn:btih:")?;
        let hex = hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        let base32 =
            hash.len() == 32 && hash.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'));
        (hex || base32).then(|| hash.to_owned())
    })
}

/// The display name (`dn`) of a magnet link.
pub fn magnet_name(uri: &str) -> Option<String> {
    let query = uri.trim().get(8..)?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key.eq_ignore_ascii_case("dn"))
        .map(|(_, value)| value.into_owned())
        .filter(|v| !v.trim().is_empty())
}

/// Shortest form of a candidate URL fit for a terminal list.
pub fn short(url: &str) -> String {
    match magnet_hash(url) {
        Some(hash) => format!("magnet:?xt=urn:btih:{hash}"),
        None => url.to_owned(),
    }
}

/// (page button key, update-server platform, label). Aliases follow their primary entry.
const VSCODE: &[(&str, &str, &str)] = &[
    (
        "win64user",
        "win32-x64-user",
        "Windows - User Installer (x64)",
    ),
    ("win", "win32-x64-user", "Windows - User Installer (x64)"),
    ("win64", "win32-x64", "Windows - System Installer (x64)"),
    ("winzip", "win32-x64-archive", "Windows - ZIP archive (x64)"),
    ("wincli", "cli-win32-x64", "Windows - CLI (x64)"),
    (
        "win32arm64user",
        "win32-arm64-user",
        "Windows - User Installer (ARM64)",
    ),
    (
        "win32arm64setup",
        "win32-arm64",
        "Windows - System Installer (ARM64)",
    ),
    (
        "win32arm64zip",
        "win32-arm64-archive",
        "Windows - ZIP archive (ARM64)",
    ),
    ("win32arm64cli", "cli-win32-arm64", "Windows - CLI (ARM64)"),
    ("osx", "darwin-universal-dmg", "macOS - DMG (Universal)"),
    ("darwinx64", "darwin-x64-dmg", "macOS - DMG (Intel)"),
    (
        "darwinarm64",
        "darwin-arm64-dmg",
        "macOS - DMG (Apple Silicon)",
    ),
    ("darwinx64cli", "cli-darwin-x64", "macOS - CLI (Intel)"),
    (
        "darwinarm64cli",
        "cli-darwin-arm64",
        "macOS - CLI (Apple Silicon)",
    ),
    ("linux64", "linux-x64", "Linux - tar.gz (x64)"),
    ("linux", "linux-x64", "Linux - tar.gz (x64)"),
    ("linuxarm64", "linux-arm64", "Linux - tar.gz (ARM64)"),
    ("linuxarmhf", "linux-armhf", "Linux - tar.gz (ARM32)"),
    (
        "linux64_deb",
        "linux-deb-x64",
        "Linux - Debian/Ubuntu .deb (x64)",
    ),
    (
        "linuxarm64_deb",
        "linux-deb-arm64",
        "Linux - Debian/Ubuntu .deb (ARM64)",
    ),
    (
        "linuxarmhf_deb",
        "linux-deb-armhf",
        "Linux - Debian/Ubuntu .deb (ARM32)",
    ),
    (
        "linux64_rpm",
        "linux-rpm-x64",
        "Linux - Fedora/Red Hat .rpm (x64)",
    ),
    (
        "linuxarm64_rpm",
        "linux-rpm-arm64",
        "Linux - Fedora/Red Hat .rpm (ARM64)",
    ),
    (
        "linuxarmhf_rpm",
        "linux-rpm-armhf",
        "Linux - Fedora/Red Hat .rpm (ARM32)",
    ),
    ("linux64cli", "cli-alpine-x64", "Linux - CLI (x64)"),
    ("linuxarm64cli", "cli-alpine-arm64", "Linux - CLI (ARM64)"),
    ("linuxarmhfcli", "cli-linux-armhf", "Linux - CLI (ARM32)"),
];

fn vscode_download(platform: &str, channel: &str) -> Url {
    Url::parse(&format!(
        "https://update.code.visualstudio.com/latest/{platform}/{channel}"
    ))
    .unwrap()
}

pub fn vscode(url: &Url) -> Option<(Url, &'static str)> {
    if url.host_str()? != "code.visualstudio.com" {
        return None;
    }
    let values: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    let key = values.get("dv")?;
    let (_, platform, label) = VSCODE.iter().find(|(button, _, _)| button == key)?;
    let channel = if values.get("build").is_some_and(|v| v == "insider") {
        "insider"
    } else {
        "stable"
    };
    Some((vscode_download(platform, channel), label))
}

/// Every official build, without fetching the (script-driven) download page.
fn vscode_all(channel: &str) -> Vec<Candidate> {
    let mut seen = std::collections::BTreeSet::new();
    VSCODE
        .iter()
        .filter(|(_, platform, _)| seen.insert(*platform))
        .map(|(_, platform, label)| {
            Candidate::new(vscode_download(platform, channel).as_str(), *label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route_of(raw: &str) -> Route {
        route(&Url::parse(raw).unwrap())
    }
    fn rewritten(raw: &str) -> String {
        match route_of(raw) {
            Route::Rewrite(c) => c.url,
            _ => panic!("{raw} was not rewritten"),
        }
    }

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

    #[test]
    fn vscode_front_page_lists_every_build_without_fetching() {
        let all = |raw: &str| match route_of(raw) {
            Route::Choices(c) => c,
            _ => panic!("{raw} should list builds"),
        };
        let stable = all("https://code.visualstudio.com/");
        assert_eq!(stable.len(), 25, "aliases must not be listed twice");
        assert!(stable.iter().all(|c| c.url.ends_with("/stable")));
        assert_eq!(
            stable[0].url,
            "https://update.code.visualstudio.com/latest/win32-x64-user/stable"
        );
        assert_eq!(all("https://code.visualstudio.com/Download").len(), 25);
        assert!(all("https://code.visualstudio.com/insiders")[3]
            .url
            .ends_with("/insider"));
        assert!(matches!(
            route_of("https://code.visualstudio.com/docs/setup"),
            Route::Generic
        ));
    }

    #[test]
    fn github_urls_map_to_scopes_and_raw_files() {
        let scope = |raw: &str| match route_of(raw) {
            Route::Api(Forge::GitHub { owner, repo, scope }) => (owner, repo, scope),
            _ => panic!("{raw} is not a GitHub API route"),
        };
        assert!(matches!(
            scope("https://github.com/BurntSushi/ripgrep"),
            (_, _, Scope::Repo)
        ));
        assert!(
            matches!(scope("https://github.com/o/r.git"), (_, ref r, Scope::Repo) if r.as_str() == "r")
        );
        assert!(matches!(
            scope("https://github.com/o/r/releases"),
            (_, _, Scope::List)
        ));
        assert!(matches!(
            scope("https://github.com/o/r/releases/latest"),
            (_, _, Scope::Latest)
        ));
        assert!(
            matches!(scope("https://github.com/o/r/releases/tag/v1.2"), (_, _, Scope::Tag(t)) if t == "v1.2")
        );
        assert!(
            matches!(scope("https://github.com/o/r/releases/tag/rel/2.0"), (_, _, Scope::Tag(t)) if t == "rel/2.0")
        );
        assert_eq!(
            rewritten("https://github.com/o/r/blob/main/docs/a%20b.txt?plain=1"),
            "https://github.com/o/r/raw/main/docs/a%20b.txt"
        );
        // Real asset and archive URLs and non-repository pages are left alone.
        for raw in [
            "https://github.com/o/r/releases/download/v1/x.zip",
            "https://github.com/o/r/archive/refs/tags/v1.zip",
            "https://github.com/topics/rust",
            "https://github.com/o",
            "https://github.com/o/r/issues/4",
        ] {
            assert!(matches!(route_of(raw), Route::Generic), "{raw}");
        }
        match route_of("https://github.com/o/r/tree/feature/x") {
            Route::Choices(c) => {
                assert_eq!(c[0].url, "https://github.com/o/r/archive/feature/x.zip");
                assert_eq!(c[1].url, "https://github.com/o/r/archive/feature/x.tar.gz");
            }
            _ => panic!("tree should offer source archives"),
        }
    }

    #[test]
    fn other_forges_are_recognised_by_url_shape() {
        assert!(matches!(
            route_of("https://gitlab.com/group/sub/proj/-/releases"),
            Route::Api(Forge::GitLab { project, scope: Scope::List, .. }) if project == "group/sub/proj"
        ));
        assert!(matches!(
            route_of("https://git.example.org/a/b/-/releases/v1.0"),
            Route::Api(Forge::GitLab { scope: Scope::Tag(t), .. }) if t == "v1.0"
        ));
        assert!(matches!(
            route_of("https://gitlab.com/group/proj"),
            Route::Api(Forge::GitLab {
                scope: Scope::Repo,
                ..
            })
        ));
        assert!(matches!(
            route_of("https://codeberg.org/o/r/releases/tag/v2"),
            Route::Api(Forge::Gitea {
                scope: Scope::Tag(_),
                ..
            })
        ));
        assert!(matches!(
            route_of("https://codeberg.org/o/r"),
            Route::Api(Forge::Gitea {
                scope: Scope::Repo,
                ..
            })
        ));
        assert!(matches!(
            route_of("https://example.org/products/changelog"),
            Route::Generic
        ));
    }

    #[test]
    fn cloud_share_links_become_direct_downloads() {
        assert_eq!(
            rewritten("https://www.dropbox.com/scl/fi/abc/f.zip?rlkey=k&dl=0"),
            "https://www.dropbox.com/scl/fi/abc/f.zip?rlkey=k&dl=1"
        );
        assert!(matches!(
            route_of("https://www.dropbox.com/scl/fi/abc/f.zip?dl=1"),
            Route::Generic
        ));
        for raw in [
            "https://drive.google.com/file/d/1AbC_dEf/view?usp=sharing",
            "https://drive.google.com/open?id=1AbC_dEf",
            "https://drive.google.com/uc?export=download&id=1AbC_dEf",
        ] {
            assert_eq!(
                rewritten(raw),
                "https://drive.usercontent.google.com/download?id=1AbC_dEf&export=download&confirm=t"
            );
        }
        assert!(matches!(
            route_of("https://drive.google.com/drive/folders/1xyz"),
            Route::Blocked(_)
        ));
        assert!(matches!(
            route_of("https://mega.nz/file/abc#key"),
            Route::Blocked(_)
        ));
        assert_eq!(
            rewritten("https://pixeldrain.com/u/AbCd1234"),
            "https://pixeldrain.com/api/file/AbCd1234?download"
        );
        match route_of("https://docs.google.com/spreadsheets/d/SHEETID/edit#gid=0") {
            Route::Choices(c) => {
                assert_eq!(
                    c[0].url,
                    "https://docs.google.com/spreadsheets/d/SHEETID/export?format=xlsx"
                );
                assert!(c[0].label.contains("Google spreadsheet"));
            }
            _ => panic!("docs export choices expected"),
        }
        let onedrive = rewritten("https://1drv.ms/u/s!AbCdEf");
        let token = onedrive
            .strip_prefix("https://api.onedrive.com/v1.0/shares/u!")
            .and_then(|t| t.strip_suffix("/root/content"))
            .unwrap();
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .unwrap();
        assert_eq!(decoded, b"https://1drv.ms/u/s!AbCdEf");
    }

    #[test]
    fn model_hubs_and_archives() {
        assert!(matches!(
            route_of("https://huggingface.co/prajjwal1/bert-tiny"),
            Route::Api(Forge::HuggingFace { dataset: false, repo, rev, .. }) if repo == "prajjwal1/bert-tiny" && rev == "main"
        ));
        assert!(matches!(
            route_of("https://huggingface.co/datasets/squad"),
            Route::Api(Forge::HuggingFace { dataset: true, repo, .. }) if repo == "squad"
        ));
        assert!(matches!(
            route_of("https://huggingface.co/o/m/tree/dev/sub/dir"),
            Route::Api(Forge::HuggingFace { rev, path, .. }) if rev == "dev" && path == "sub/dir"
        ));
        assert_eq!(
            rewritten("https://huggingface.co/o/m/blob/main/dir/model.gguf"),
            "https://huggingface.co/o/m/resolve/main/dir/model.gguf?download=true"
        );
        assert!(matches!(
            route_of("https://huggingface.co/docs/hub"),
            Route::Generic
        ));
        assert!(matches!(
            route_of("https://archive.org/details/test_item_1"),
            Route::Api(Forge::ArchiveOrg { id }) if id == "test_item_1"
        ));
        assert!(matches!(
            route_of("https://sourceforge.net/projects/sevenzip/"),
            Route::Api(Forge::SourceForge { project }) if project == "sevenzip"
        ));
        assert!(matches!(
            route_of("https://sourceforge.net/projects/sevenzip/files/7-Zip/"),
            Route::Generic
        ));
    }

    #[test]
    fn media_pages_are_recognised_but_site_pages_are_not() {
        for raw in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL1",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/abc",
            "https://vimeo.com/76979871",
            "https://x.com/user/status/1234",
            "https://www.instagram.com/reel/Cabc/",
            "https://www.tiktok.com/@u/video/1",
            "https://www.reddit.com/r/videos/comments/abc/title/",
            "https://soundcloud.com/artist/track",
            "https://artist.bandcamp.com/track/song",
        ] {
            assert!(matches!(route_of(raw), Route::Media), "{raw}");
        }
        for raw in [
            "https://www.youtube.com/about/",
            "https://vimeo.com/features",
            "https://soundcloud.com/discover",
            "https://x.com/home",
        ] {
            assert!(matches!(route_of(raw), Route::Generic), "{raw}");
        }
    }

    #[test]
    fn magnet_links_are_validated() {
        let hash = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
        let good = format!(
            "magnet:?xt=urn:btih:{}&dn=Ubuntu+24.04&tr=udp%3A%2F%2Ft.example%3A1337",
            hash.to_uppercase()
        );
        assert_eq!(magnet_hash(&good).as_deref(), Some(hash));
        assert_eq!(magnet_name(&good).as_deref(), Some("Ubuntu 24.04"));
        assert_eq!(short(&good), format!("magnet:?xt=urn:btih:{hash}"));
        assert!(magnet_hash("magnet:?xt=urn:btih:short").is_none());
        assert!(magnet_hash("magnet:?dn=no-hash").is_none());
        assert!(magnet_hash(
            "https://example.com/?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a"
        )
        .is_none());
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
