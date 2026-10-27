use crate::http;
use anyhow::{bail, ensure, Result};
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Serialize;
use std::collections::BTreeMap;
use url::Url;

#[derive(Serialize)]
pub struct Candidate {
    pub url: String,
    pub label: String,
}

pub async fn discover(client: &Client, raw: &str) -> Result<Vec<Candidate>> {
    let url = http::valid_url(raw)?;
    let (mut response, _) = http::request(client.get(url), 2).await?;
    response.error_for_status_ref()?;
    if !http::header(&response, "content-type")
        .unwrap_or("")
        .to_ascii_lowercase()
        .contains("text/html")
    {
        return Ok(vec![Candidate {
            url: response.url().to_string(),
            label: "direct download".into(),
        }]);
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

fn extract(text: &str, base: &Url) -> Vec<Candidate> {
    let document = Html::parse_document(text);
    let base_selector = Selector::parse("base[href]").unwrap();
    let base = document
        .select(&base_selector)
        .next()
        .and_then(|b| base.join(b.value().attr("href")?).ok())
        .unwrap_or_else(|| base.clone());
    let selector = Selector::parse("a[href]").unwrap();
    let mut links = BTreeMap::new();
    for link in document.select(&selector) {
        let Some(href) = link.value().attr("href") else {
            continue;
        };
        let Ok(mut url) = base.join(href) else {
            continue;
        };
        if http::valid_url(url.as_str()).is_err() {
            continue;
        }
        url.set_fragment(None);
        let path = url.path().to_ascii_lowercase();
        let suffixes = [
            ".zip",
            ".iso",
            ".tar",
            ".gz",
            ".xz",
            ".bz2",
            ".7z",
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
            ".AppImage",
        ];
        if link.value().attr("download").is_some()
            || suffixes
                .iter()
                .any(|s| path.ends_with(&s.to_ascii_lowercase()))
        {
            links.entry(url.to_string()).or_insert_with(|| {
                link.text()
                    .collect::<String>()
                    .trim()
                    .chars()
                    .take(120)
                    .collect::<String>()
            });
        }
    }
    links
        .into_iter()
        .take(200)
        .map(|(url, label)| Candidate { url, label })
        .collect()
}

pub async fn select(client: &Client, raw: &str) -> Result<Url> {
    // Probe without consuming the direct file; no extra full GET for ordinary URLs.
    let url = http::valid_url(raw)?;
    let info = http::probe(client, url.clone(), 2).await?;
    if !info.content_type.to_ascii_lowercase().contains("text/html") {
        return http::valid_url(&info.url);
    }
    let links = discover(client, raw).await?;
    if links.len() == 1 {
        return http::valid_url(&links[0].url);
    }
    bail!("Page has {} download candidates. Run 'fluxget resolve {url}' and choose a URL, or use --direct to save the page", links.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_and_deduplicates() {
        let links = extract(
            r#"<base href="/files/"><a href="x.zip">ZIP</a><a href="x.zip#copy">copy</a><a download href="../blob?id=1">file</a><a href="javascript:evil()" download>bad</a><a href="/about">about</a>"#,
            &Url::parse("https://example.com/page").unwrap(),
        );
        assert_eq!(links.len(), 2);
        assert!(links
            .iter()
            .any(|c| c.url == "https://example.com/files/x.zip"));
    }
}
