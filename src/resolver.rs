use crate::{
    forges,
    http::{self, Fetched, Info},
    picker,
    sites::{self, Route},
};
use anyhow::{bail, ensure, Result};
use base64::Engine as _;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Serialize;
use std::collections::BTreeSet;
use url::Url;

/// Which engine downloads a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// fluxget's native segmented HTTP(S) engine.
    #[default]
    Http,
    /// A magnet link or .torrent, downloaded by aria2.
    Torrent,
    /// A video/audio page or stream playlist, downloaded by yt-dlp.
    Media,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub url: String,
    pub label: String,
    pub kind: Kind,
    /// Publisher-supplied SHA-256 (release API digests); verified before publishing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl Candidate {
    pub fn new(url: impl Into<String>, label: impl Into<String>) -> Self {
        let url = url.into();
        Self {
            kind: kind_of(&url),
            url,
            label: label.into(),
            sha256: None,
        }
    }
    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }
    pub fn sha256(mut self, sha256: Option<String>) -> Self {
        self.sha256 = sha256;
        self
    }
}

/// What a finished resolution hands to the downloader.
pub struct Target {
    pub kind: Kind,
    pub url: String,
    pub sha256: Option<String>,
    /// The probe that already established size and range support.
    pub info: Option<Info>,
}

fn kind_of(url: &str) -> Kind {
    if sites::magnet_hash(url).is_some() {
        return Kind::Torrent;
    }
    let Ok(parsed) = Url::parse(url) else {
        return Kind::Http;
    };
    let path = parsed.path().to_ascii_lowercase();
    if path.ends_with(".torrent") || path.contains("/torrent/download/") {
        Kind::Torrent
    } else if path.ends_with(".m3u8") || path.ends_with(".mpd") {
        Kind::Media
    } else {
        Kind::Http
    }
}

fn kind_of_file(info: &Info) -> Kind {
    let content_type = info.content_type.to_ascii_lowercase();
    let name = info.filename.to_ascii_lowercase();
    if content_type.contains("x-bittorrent") || name.ends_with(".torrent") {
        Kind::Torrent
    } else if content_type.contains("mpegurl")
        || content_type.contains("dash+xml")
        || name.ends_with(".m3u8")
        || name.ends_with(".mpd")
    {
        Kind::Media
    } else {
        Kind::Http
    }
}

pub struct Page {
    pub links: Vec<Candidate>,
    pub indexes: Vec<Url>,
}

enum Step {
    File(Info),
    Choose(Vec<Candidate>),
}

/// One resolution step: a file was reached, or there are choices to make.
async fn step(
    client: &Client,
    url: &Url,
    retries: u8,
    prefetched: Option<Fetched>,
) -> Result<Step> {
    match sites::route(url) {
        Route::Rewrite(candidate) => return Ok(Step::Choose(vec![candidate])),
        Route::Choices(candidates) => return Ok(Step::Choose(candidates)),
        Route::Media => {
            let label = "Media page (downloaded with yt-dlp)";
            return Ok(Step::Choose(vec![
                Candidate::new(url.as_str(), label).kind(Kind::Media)
            ]));
        }
        Route::Blocked(reason) => bail!("{reason}"),
        Route::Api(forge) => {
            if let Some(candidates) = forges::expand(client, &forge).await? {
                return Ok(Step::Choose(candidates));
            }
        }
        Route::Generic => {}
    }
    let fetched = match prefetched {
        Some(fetched) => fetched,
        None => http::fetch(client, url.clone(), retries).await?,
    };
    match fetched {
        Fetched::File(info) => Ok(Step::File(info)),
        Fetched::Page { url, body } => {
            let links = page_candidates(client, &url, &body, retries).await;
            if links.is_empty() && builds_link_with_script(&body) {
                bail!(
                    "{} builds its download link with JavaScript (an edition or language form), so there is no link in the page to find. \
                     Choose the download in a browser, copy the link it gives you, and run 'fluxget <that link>'",
                    url.host_str().unwrap_or("This page")
                );
            }
            Ok(Step::Choose(links))
        }
    }
}

/// A download form (a dropdown plus a script) that produces its link on the
/// client, as Microsoft's software-download pages do.
fn builds_link_with_script(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("<select") && body.contains("<script") && body.contains("download")
}

/// Links on a page, plus those on up to three same-origin download indexes,
/// fetched concurrently so they cost one round trip rather than three.
async fn page_candidates(client: &Client, url: &Url, body: &str, retries: u8) -> Vec<Candidate> {
    let page = extract(body, url);
    let mut links = page.links;
    let mut seen: BTreeSet<String> = links.iter().map(|c| c.url.clone()).collect();
    let here = url.as_str().to_ascii_lowercase();
    let mut fetches = tokio::task::JoinSet::new();
    for (position, index) in page
        .indexes
        .into_iter()
        .filter(|i| i.as_str().to_ascii_lowercase() != here)
        .take(3)
        .enumerate()
    {
        let client = client.clone();
        fetches.spawn(async move { (position, http::fetch(&client, index, retries).await) });
    }
    let mut found: Vec<(usize, Vec<Candidate>)> = Vec::new();
    while let Some(Ok((position, result))) = fetches.join_next().await {
        match result {
            Ok(Fetched::File(info)) => {
                let label = format!("Download: {}", info.filename);
                found.push((
                    position,
                    vec![Candidate::new(info.url.clone(), label).kind(kind_of_file(&info))],
                ));
            }
            Ok(Fetched::Page { url, body }) => found.push((position, extract(&body, &url).links)),
            Err(_) => {}
        }
    }
    found.sort_by_key(|(position, _)| *position);
    for candidate in found.into_iter().flat_map(|(_, c)| c) {
        if links.len() < 200 && seen.insert(candidate.url.clone()) {
            links.push(candidate);
        }
    }
    links.truncate(200);
    links
}

/// Every download the page at `raw` offers, without choosing one.
pub async fn discover(client: &Client, raw: &str) -> Result<Vec<Candidate>> {
    let url = http::valid_url(raw)?;
    Ok(match step(client, &url, 2, None).await? {
        Step::File(info) => {
            let kind = kind_of_file(&info);
            vec![Candidate::new(info.url, "Direct download").kind(kind)]
        }
        Step::Choose(candidates) => candidates,
    })
}

fn extension(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or("")
}

fn file_suffix(path: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        // archives, disk images, installers and packages
        "zip",
        "iso",
        "tar",
        "gz",
        "tgz",
        "xz",
        "txz",
        "bz2",
        "zst",
        "7z",
        "rar",
        "exe",
        "msi",
        "msix",
        "msixbundle",
        "appx",
        "dmg",
        "pkg",
        "deb",
        "rpm",
        "appimage",
        "apk",
        "xapk",
        "aab",
        "ipa",
        "snap",
        "flatpakref",
        "vsix",
        "jar",
        "whl",
        "nupkg",
        "crx",
        "xpi",
        "img",
        "vhd",
        "vhdx",
        "vmdk",
        "ova",
        "bin",
        "run",
        "sh",
        "ps1",
        // documents and data
        "pdf",
        "epub",
        "mobi",
        "azw3",
        "cbz",
        "cbr",
        "doc",
        "docx",
        "xls",
        "xlsx",
        "ppt",
        "pptx",
        "odt",
        "ods",
        "rtf",
        "csv",
        "json",
        "txt",
        "srt",
        // audio, video, images and models
        "mp4",
        "m4v",
        "mkv",
        "avi",
        "mov",
        "wmv",
        "webm",
        "mp3",
        "m4a",
        "wav",
        "flac",
        "ogg",
        "opus",
        "png",
        "jpg",
        "jpeg",
        "gif",
        "webp",
        "svg",
        "heic",
        "ttf",
        "otf",
        "gguf",
        "safetensors",
        "ckpt",
        "onnx",
        // torrents and stream playlists
        "torrent",
        "m3u8",
        "mpd",
    ];
    let path = path.to_ascii_lowercase();
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.contains('.') && SUFFIXES.contains(&extension(name)))
}

fn download_index(url: &Url) -> bool {
    let path = url.path().trim_end_matches('/').to_ascii_lowercase();
    matches!(
        path.rsplit('/').next().unwrap_or(""),
        "download" | "downloads" | "releases"
    ) && url.query().is_none()
}

/// `https://host/projects/p/files/dir/name.exe/download` style file endpoints.
fn named_download(url: &Url) -> bool {
    let mut parts = url.path().trim_end_matches('/').rsplit('/');
    parts.next() == Some("download") && parts.next().is_some_and(file_suffix)
}

fn meta_refresh(document: &Html, base: &Url) -> Option<Url> {
    let selector = Selector::parse("meta[http-equiv]").unwrap();
    document.select(&selector).find_map(|meta| {
        let element = meta.value();
        if !element.attr("http-equiv")?.eq_ignore_ascii_case("refresh") {
            return None;
        }
        let content = element.attr("content")?;
        let at = content.to_ascii_lowercase().find("url=")?;
        let target = content[at + 4..].trim().trim_matches(['\'', '"']);
        base.join(target).ok()
    })
}

pub fn extract(text: &str, origin: &Url) -> Page {
    let document = Html::parse_document(text);
    let base_selector = Selector::parse("base[href]").unwrap();
    let base = document
        .select(&base_selector)
        .next()
        .and_then(|b| origin.join(b.value().attr("href")?).ok())
        .unwrap_or_else(|| origin.clone());
    let selector = Selector::parse(
        "a[href], [data-download-url], [data-url], [data-href], [data-scrambled-url], \
         button[data-os], video[src], audio[src], source[src], \
         meta[property=\"og:video\"], meta[property=\"og:video:url\"], \
         meta[property=\"og:video:secure_url\"], meta[property=\"og:audio\"]",
    )
    .unwrap();
    let mut links: Vec<Candidate> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut indexes = std::collections::BTreeMap::new();
    let mut push = |links: &mut Vec<Candidate>, candidate: Candidate| {
        let key = sites::magnet_hash(&candidate.url).unwrap_or_else(|| candidate.url.clone());
        if seen.insert(key) {
            links.push(candidate);
        }
    };
    for element in document.select(&selector) {
        let attributes = element.value();
        let text = element.text().collect::<String>();
        let label = picker::plain(attributes.attr("aria-label").unwrap_or(&text));
        let tag = attributes.name();
        // Embedded media players and social-preview tags name the file itself.
        let embedded = match tag {
            "video" | "audio" | "source" => attributes.attr("src"),
            "meta" => attributes.attr("content"),
            _ => None,
        };
        if let Some(src) = embedded {
            if let Ok(mut url) = base.join(src.trim()) {
                url.set_fragment(None);
                if http::valid_url(url.as_str()).is_ok() {
                    let kind = kind_of(url.as_str());
                    let name = url
                        .path_segments()
                        .and_then(|mut p| p.next_back())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("media");
                    let label =
                        format!("{} {name}", if tag == "audio" { "Audio" } else { "Video" });
                    push(&mut links, Candidate::new(url.as_str(), label).kind(kind));
                }
            }
            continue;
        }
        let hint =
            attributes.attr("download").is_some() || attributes.attr("data-download-url").is_some();
        let scrambled = attributes.attr("data-scrambled-url").and_then(|v| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(v.trim())
                .ok()?;
            String::from_utf8(bytes).ok()
        });
        let href = scrambled.as_deref().or_else(|| {
            attributes
                .attr("href")
                .or_else(|| attributes.attr("data-download-url"))
                .or_else(|| attributes.attr("data-url"))
                .or_else(|| attributes.attr("data-href"))
        });
        if let Some(raw) = href.map(str::trim) {
            if raw.len() >= 8 && raw[..8].eq_ignore_ascii_case("magnet:?") {
                if sites::magnet_hash(raw).is_some() {
                    let name = sites::magnet_name(raw).map(|n| picker::plain(&n));
                    let label = name
                        .or(Some(label).filter(|l| !l.is_empty()))
                        .unwrap_or_else(|| "Magnet link".into());
                    push(&mut links, Candidate::new(raw, label).kind(Kind::Torrent));
                }
                continue;
            }
        }
        let button_url = if href.is_none() && origin.host_str() == Some("code.visualstudio.com") {
            attributes.attr("data-os").map(|os| {
                let mut url = origin.join("/thank-you").unwrap();
                url.query_pairs_mut()
                    .append_pair("dv", os)
                    .append_pair("build", "stable");
                url
            })
        } else {
            None
        };
        let url = href.and_then(|href| base.join(href).ok()).or(button_url);
        let Some(mut url) = url else {
            continue;
        };
        if http::valid_url(url.as_str()).is_err() {
            continue;
        }
        url.set_fragment(None);
        if url == *origin {
            continue;
        }
        if let Some((mapped, label)) = sites::vscode(&url) {
            push(&mut links, Candidate::new(mapped.as_str(), label));
            continue;
        }
        let named = named_download(&url);
        if !named && download_index(&url) {
            if url.origin() == origin.origin() {
                indexes.insert(url.as_str().to_ascii_lowercase(), url);
            }
            continue;
        }
        let filename_query = url.query_pairs().any(|(key, value)| {
            matches!(key.as_ref(), "filename" | "file" | "download" | "name") && file_suffix(&value)
        });
        let endpoint = url
            .path()
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let download_service = url
            .host_str()
            .is_some_and(|host| host.starts_with("download"));
        let lowered = label.to_ascii_lowercase();
        let text_hint = (lowered.contains("download") || lowered.contains("torrent"))
            && (matches!(
                endpoint.as_str(),
                "get" | "fetch" | "attachment" | "download.php"
            ) || download_service)
            && url.path() != origin.path();
        let path_hint = url.path().contains("/releases/download/")
            || url
                .path()
                .to_ascii_lowercase()
                .contains("/torrent/download/");
        if hint || named || path_hint || file_suffix(url.path()) || filename_query || text_hint {
            let label = if label.is_empty() {
                url.path_segments()
                    .into_iter()
                    .flatten()
                    .rev()
                    .find(|s| !s.is_empty() && *s != "download")
                    .unwrap_or("Download")
                    .to_owned()
            } else {
                label
            };
            push(&mut links, Candidate::new(url.as_str(), label));
        }
    }
    if links.is_empty() {
        // "Your download will start shortly" pages forward the browser with a meta refresh.
        if let Some(target) = meta_refresh(&document, &base) {
            if http::valid_url(target.as_str()).is_ok() && target != *origin {
                links.push(Candidate::new(
                    target.as_str(),
                    "Automatic download redirect",
                ));
            }
        }
    }
    links.truncate(200);
    Page {
        links,
        indexes: indexes.into_values().take(3).collect(),
    }
}

/// Resolve `raw` to something downloadable, asking the user to choose when
/// several downloads are offered.
pub async fn select(
    client: &Client,
    raw: &str,
    mut selection: Option<usize>,
    mut force_picker: bool,
    json: bool,
) -> Result<Option<Target>> {
    if let Some(target) = local_source(raw) {
        return Ok(Some(target));
    }
    let mut url = http::valid_url(raw)?;
    let mut visited = BTreeSet::new();
    let mut digest = None;
    let mut prefetched = None;
    for _ in 0..4 {
        ensure!(
            visited.insert(url.to_string()),
            "Download landing-page loop detected"
        );
        let links = match step(client, &url, 2, prefetched.take()).await? {
            Step::File(info) => {
                let kind = kind_of_file(&info);
                return Ok(Some(Target {
                    kind,
                    url: info.url.clone(),
                    sha256: digest,
                    info: (kind == Kind::Http).then_some(info),
                }));
            }
            Step::Choose(links) => links,
        };
        // While the person decides, fetch the highlighted download: its DNS,
        // TLS and redirects are done, or its answer is already here, on Enter.
        let guess = picker::will_prompt(&links, selection, force_picker, json)
            .then(|| picker::default_index(&links))
            .and_then(|i| speculate(client, &links[i]).map(|task| (i, task)));
        let choice = picker::choose(&links, selection.take(), force_picker, json);
        force_picker = false;
        if let Some((guessed, task)) = guess {
            match choice {
                Ok(Some(index)) if index == guessed => {
                    prefetched = task.await.ok().and_then(Result::ok);
                }
                _ => task.abort(),
            }
        }
        let Some(index) = choice? else {
            return Ok(None);
        };
        let chosen = &links[index];
        if chosen.kind != Kind::Http {
            return Ok(Some(Target {
                kind: chosen.kind,
                url: chosen.url.clone(),
                sha256: None,
                info: None,
            }));
        }
        digest = chosen.sha256.clone().or(digest);
        url = http::valid_url(&chosen.url)?;
    }
    bail!("Too many download landing pages. Run 'fluxget resolve URL' and choose a direct file URL")
}

/// Start the request `step` would make for a plain web candidate.
fn speculate(
    client: &Client,
    candidate: &Candidate,
) -> Option<tokio::task::JoinHandle<Result<Fetched>>> {
    let url = http::valid_url(&candidate.url).ok()?;
    if candidate.kind != Kind::Http || !matches!(sites::route(&url), Route::Generic) {
        return None;
    }
    let client = client.clone();
    Some(tokio::spawn(
        async move { http::fetch(&client, url, 0).await },
    ))
}

/// Inputs that are not web pages: a magnet link or a `.torrent` file on disk.
fn local_source(raw: &str) -> Option<Target> {
    let torrent = |url: &str| Target {
        kind: Kind::Torrent,
        url: url.to_owned(),
        sha256: None,
        info: None,
    };
    if sites::magnet_hash(raw).is_some() {
        return Some(torrent(raw.trim()));
    }
    let path = std::path::Path::new(raw);
    (raw.to_ascii_lowercase().ends_with(".torrent") && path.is_file()).then(|| torrent(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page(html: &str, origin: &str) -> Page {
        extract(html, &Url::parse(origin).unwrap())
    }
    #[test]
    fn resolves_and_deduplicates() {
        let page = page(
            r#"<base href="/files/"><a href="x.zip">ZIP</a><a href="x.zip#copy">copy</a><a download href="../blob?id=1">file</a><a href="javascript:evil()" download>bad</a><a href="/about">about</a>"#,
            "https://example.com/page",
        );
        assert_eq!(page.links.len(), 2);
        assert!(page
            .links
            .iter()
            .any(|c| c.url == "https://example.com/files/x.zip"));
    }
    #[test]
    fn finds_same_origin_indexes_query_and_data_links() {
        let page = page(
            r#"<a href="/download">Downloads</a><a href="https://other.example/download">Other</a><button data-download-url="/blob?filename=report.pdf">Report</button><a href="/fetch?file=data.csv">Data</a>"#,
            "https://example.com/",
        );
        assert_eq!(page.indexes.len(), 1);
        assert_eq!(page.links.len(), 2);
    }
    #[test]
    fn vscode_buttons_become_real_platform_downloads() {
        let page = page(
            r#"<button data-os="win">Windows</button><a href="/thank-you?dv=win64user&amp;build=stable">User Installer</a><a href="/thank-you?dv=darwinarm64&amp;build=stable">Apple Silicon</a>"#,
            "https://code.visualstudio.com/",
        );
        assert_eq!(page.links.len(), 2);
        assert!(page
            .links
            .iter()
            .all(|c| c.url.starts_with("https://update.code.visualstudio.com/")));
    }
    #[test]
    fn torrent_site_pages_yield_magnets_and_torrent_files_in_page_order() {
        let hash = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
        let html = format!(
            r#"<a href="/download/9.torrent"><i class="icon"></i></a>
               <a href="magnet:?xt=urn:btih:{hash}&amp;dn=Linux+ISO&amp;tr=udp%3A%2F%2Ftracker.example%3A80"><i></i></a>
               <a href="MAGNET:?xt=urn:btih:{upper}&dn=duplicate">again</a>
               <a href="magnet:?xt=urn:btih:nothex">broken</a>
               <a href="https://yts.example/torrent/download/Movie-2021-1080p">1080p</a>
               <a href="/about">About</a>"#,
            upper = hash.to_uppercase()
        );
        let page = page(&html, "https://tracker.example/t/9");
        let kinds: Vec<_> = page
            .links
            .iter()
            .map(|c| (c.kind, c.label.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (Kind::Torrent, "9.torrent"),
                (Kind::Torrent, "Linux ISO"),
                (Kind::Torrent, "1080p")
            ]
        );
        assert!(page.links[1].url.starts_with("magnet:?xt=urn:btih:"));
    }
    #[test]
    fn modern_download_pages_are_understood() {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode("https://download123.mediafire.example/x/file.zip");
        let html = format!(
            r#"<a id="downloadButton" aria-label="Download file" data-scrambled-url="{encoded}" href="javascript:void(0)">Download</a>
               <video src="/clips/intro.mp4"></video>
               <video><source src="https://cdn.example/live/stream.m3u8"></video>
               <meta property="og:video" content="https://cdn.example/v/clip.webm">
               <a href="/projects/p/files/dir/app.exe/download">app.exe</a>
               <a href="https://github.com/o/r/releases/download/v1/tool-linux">tool-linux</a>"#
        );
        let page = page(&html, "https://example.org/watch");
        let urls: Vec<_> = page
            .links
            .iter()
            .map(|c| (c.url.as_str(), c.kind))
            .collect();
        assert!(urls.contains(&(
            "https://download123.mediafire.example/x/file.zip",
            Kind::Http
        )));
        assert!(urls.contains(&("https://example.org/clips/intro.mp4", Kind::Http)));
        assert!(urls.contains(&("https://cdn.example/live/stream.m3u8", Kind::Media)));
        assert!(urls.contains(&("https://cdn.example/v/clip.webm", Kind::Http)));
        assert!(urls.contains(&(
            "https://example.org/projects/p/files/dir/app.exe/download",
            Kind::Http
        )));
        assert!(urls.contains(&(
            "https://github.com/o/r/releases/download/v1/tool-linux",
            Kind::Http
        )));
        assert!(page.indexes.is_empty());
    }
    #[test]
    fn meta_refresh_is_only_a_fallback() {
        let refresh = r#"<meta http-equiv="Refresh" content="3; URL='/get/file.bin'">Your download will start shortly"#;
        let page_with_only_refresh = page(refresh, "https://example.org/thanks");
        assert_eq!(page_with_only_refresh.links.len(), 1);
        assert_eq!(
            page_with_only_refresh.links[0].url,
            "https://example.org/get/file.bin"
        );
        let with_links = page(
            &format!("{refresh}<a href=\"a.zip\">a</a>"),
            "https://example.org/thanks",
        );
        assert_eq!(with_links.links.len(), 1);
        assert!(with_links.links[0].url.ends_with("/a.zip"));
    }
    #[test]
    fn kinds_follow_file_names_and_content_types() {
        assert_eq!(kind_of("https://x.example/a/b.TORRENT"), Kind::Torrent);
        assert_eq!(kind_of("https://x.example/live.m3u8?token=1"), Kind::Media);
        assert_eq!(kind_of("https://x.example/a.zip"), Kind::Http);
        assert_eq!(
            kind_of("magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a"),
            Kind::Torrent
        );
        let info = |ct: &str, name: &str| Info {
            url: "https://x.example/f".into(),
            size: None,
            ranges: false,
            etag: None,
            filename: name.into(),
            content_type: ct.into(),
            protocol: "HTTP/1.1".into(),
        };
        assert_eq!(
            kind_of_file(&info("application/x-bittorrent", "f")),
            Kind::Torrent
        );
        assert_eq!(
            kind_of_file(&info("application/octet-stream", "ubuntu.torrent")),
            Kind::Torrent
        );
        assert_eq!(
            kind_of_file(&info("application/vnd.apple.mpegurl", "f")),
            Kind::Media
        );
        assert_eq!(kind_of_file(&info("application/zip", "f.zip")), Kind::Http);
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn speculative_fetch_is_reused_without_another_request() {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let seen = seen.clone();
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut buf = [0u8; 2048];
                    while stream.read(&mut buf).is_ok_and(|n| n > 0) {
                        seen.fetch_add(1, Ordering::SeqCst);
                        let reply = "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/5\r\nContent-Length: 1\r\nContent-Type: application/zip\r\nETag: \"abc\"\r\n\r\nx";
                        if stream.write_all(reply.as_bytes()).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let client = reqwest::Client::new();
        let candidate = Candidate::new(format!("http://127.0.0.1:{port}/a.zip"), "a");
        let fetched = speculate(&client, &candidate)
            .expect("plain web candidates are fetched ahead")
            .await
            .unwrap()
            .unwrap();
        let url = Url::parse(&candidate.url).unwrap();
        let outcome = step(&client, &url, 0, Some(fetched)).await.unwrap();
        assert!(matches!(outcome, Step::File(info) if info.size == Some(5)));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(speculate(
            &client,
            &Candidate::new(
                "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a",
                "m"
            )
        )
        .is_none());
        assert!(speculate(&client, &Candidate::new("https://github.com/o/r", "repo")).is_none());
    }
    #[test]
    fn magnets_and_local_torrents_skip_the_web() {
        let target =
            local_source("magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=x")
                .unwrap();
        assert_eq!(target.kind, Kind::Torrent);
        assert!(local_source("magnet:?dn=nohash").is_none());
        assert!(local_source("missing-file.torrent").is_none());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.torrent");
        std::fs::write(&file, b"d4:infod4:name1:aee").unwrap();
        assert_eq!(
            local_source(file.to_str().unwrap()).unwrap().kind,
            Kind::Torrent
        );
    }
}
