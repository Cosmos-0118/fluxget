use crate::{http, picker, sites};
use anyhow::{bail, ensure, Result};
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub url: String,
    pub label: String,
}

fn html_type(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("text/html") || value.contains("application/xhtml+xml")
}

struct Page {
    links: Vec<Candidate>,
    indexes: Vec<Url>,
}

async fn read_page(client: &Client, url: Url) -> Result<Page> {
    let (mut response, _) = http::request(client.get(url), 2).await?;
    response.error_for_status_ref()?;
    if !html_type(http::header(&response, "content-type").unwrap_or("")) {
        return Ok(Page {
            links: vec![Candidate {
                url: response.url().to_string(),
                label: "Direct download".into(),
            }],
            indexes: vec![],
        });
    }
    let final_url = response.url().clone();
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            data.len() + chunk.len() <= 2 * 1024 * 1024,
            "HTML page exceeds 2 MiB resolver limit; use --direct to save it"
        );
        data.extend_from_slice(&chunk);
    }
    Ok(extract(&String::from_utf8_lossy(&data), &final_url))
}

pub async fn discover(client: &Client, raw: &str) -> Result<Vec<Candidate>> {
    let url = http::valid_url(raw)?;
    let page = read_page(client, url.clone()).await?;
    let mut links: BTreeMap<String, String> =
        page.links.into_iter().map(|c| (c.url, c.label)).collect();
    let mut visited = BTreeSet::from([url.as_str().to_ascii_lowercase()]);
    // One level of same-origin download indexes, at most four pages in total.
    for index in page.indexes {
        if visited.len() >= 4 || links.len() >= 200 {
            break;
        }
        if !visited.insert(index.as_str().to_ascii_lowercase()) {
            continue;
        }
        if let Ok(child) = read_page(client, index).await {
            for candidate in child.links {
                links.insert(candidate.url, candidate.label);
            }
        }
    }
    Ok(links
        .into_iter()
        .take(200)
        .map(|(url, label)| Candidate { url, label })
        .collect())
}

fn file_suffix(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    [
        ".zip",
        ".iso",
        ".tar",
        ".gz",
        ".xz",
        ".bz2",
        ".7z",
        ".rar",
        ".exe",
        ".msi",
        ".dmg",
        ".pkg",
        ".deb",
        ".rpm",
        ".pdf",
        ".bin",
        ".mp4",
        ".mp3",
        ".whl",
        ".appimage",
        ".apk",
        ".epub",
        ".csv",
        ".xlsx",
        ".docx",
        ".pptx",
        ".json",
        ".txt",
        ".wav",
        ".flac",
        ".ogg",
        ".webm",
        ".mkv",
        ".png",
        ".jpg",
        ".jpeg",
        ".webp",
        ".svg",
    ]
    .iter()
    .any(|suffix| path.ends_with(suffix))
}

fn download_index(url: &Url) -> bool {
    let path = url.path().trim_end_matches('/').to_ascii_lowercase();
    matches!(
        path.rsplit('/').next().unwrap_or(""),
        "download" | "downloads" | "releases"
    ) && url.query().is_none()
}

fn extract(text: &str, origin: &Url) -> Page {
    let document = Html::parse_document(text);
    let base_selector = Selector::parse("base[href]").unwrap();
    let base = document
        .select(&base_selector)
        .next()
        .and_then(|b| origin.join(b.value().attr("href")?).ok())
        .unwrap_or_else(|| origin.clone());
    let selector =
        Selector::parse("a[href], [data-download-url], [data-url], [data-href], button[data-os]")
            .unwrap();
    let mut links = BTreeMap::new();
    let mut indexes = BTreeMap::new();
    for element in document.select(&selector) {
        let attributes = element.value();
        let text = element.text().collect::<String>();
        let label = picker::plain(attributes.attr("aria-label").unwrap_or(&text));
        let hint =
            attributes.attr("download").is_some() || attributes.attr("data-download-url").is_some();
        let href = attributes
            .attr("href")
            .or_else(|| attributes.attr("data-download-url"))
            .or_else(|| attributes.attr("data-url"))
            .or_else(|| attributes.attr("data-href"));
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
            links.insert(mapped.to_string(), label.into());
            continue;
        }
        if download_index(&url) {
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
            .is_some_and(|host| host.starts_with("download.") || host.starts_with("downloads."));
        let text_hint = label.to_ascii_lowercase().contains("download")
            && (matches!(
                endpoint.as_str(),
                "get" | "fetch" | "attachment" | "download.php"
            ) || download_service)
            && url.path() != origin.path();
        if hint || file_suffix(url.path()) || filename_query || text_hint {
            let label = if label.is_empty() {
                url.path_segments()
                    .and_then(|mut p| p.next_back())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Download")
                    .to_owned()
            } else {
                label
            };
            links.entry(url.to_string()).or_insert(label);
        }
    }
    Page {
        links: links
            .into_iter()
            .take(200)
            .map(|(url, label)| Candidate { url, label })
            .collect(),
        indexes: indexes.into_values().take(3).collect(),
    }
}

pub async fn select(
    client: &Client,
    raw: &str,
    mut selection: Option<usize>,
    mut force_picker: bool,
    json: bool,
) -> Result<Option<Url>> {
    let mut url = http::valid_url(raw)?;
    let mut visited = BTreeSet::new();
    for _ in 0..4 {
        ensure!(
            visited.insert(url.to_string()),
            "Download landing-page loop detected"
        );
        let info = http::probe(client, url.clone(), 2).await?;
        if !html_type(&info.content_type) {
            return Ok(Some(http::valid_url(&info.url)?));
        }
        let links = discover(client, &info.url).await?;
        let Some(index) = picker::choose(&links, selection.take(), force_picker, json)? else {
            return Ok(None);
        };
        force_picker = false;
        url = http::valid_url(&links[index].url)?;
    }
    bail!("Too many download landing pages. Run 'fluxget resolve URL' and choose a direct file URL")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_and_deduplicates() {
        let page = extract(
            r#"<base href="/files/"><a href="x.zip">ZIP</a><a href="x.zip#copy">copy</a><a download href="../blob?id=1">file</a><a href="javascript:evil()" download>bad</a><a href="/about">about</a>"#,
            &Url::parse("https://example.com/page").unwrap(),
        );
        assert_eq!(page.links.len(), 2);
        assert!(page
            .links
            .iter()
            .any(|c| c.url == "https://example.com/files/x.zip"));
    }
    #[test]
    fn finds_same_origin_indexes_query_and_data_links() {
        let page = extract(
            r#"<a href="/download">Downloads</a><a href="https://other.example/download">Other</a><button data-download-url="/blob?filename=report.pdf">Report</button><a href="/fetch?file=data.csv">Data</a>"#,
            &Url::parse("https://example.com/").unwrap(),
        );
        assert_eq!(page.indexes.len(), 1);
        assert_eq!(page.links.len(), 2);
    }
    #[test]
    fn vscode_buttons_become_real_platform_downloads() {
        let page = extract(
            r#"<button data-os="win">Windows</button><a href="/thank-you?dv=win64user&amp;build=stable">User Installer</a><a href="/thank-you?dv=darwinarm64&amp;build=stable">Apple Silicon</a>"#,
            &Url::parse("https://code.visualstudio.com/").unwrap(),
        );
        assert_eq!(page.links.len(), 2);
        assert!(page
            .links
            .iter()
            .all(|c| c.url.starts_with("https://update.code.visualstudio.com/")));
    }
}
