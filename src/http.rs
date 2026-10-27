use crate::Network;
use anyhow::{bail, ensure, Context, Result};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client, Response, StatusCode,
};
use serde::Serialize;
use std::time::{Duration, SystemTime};
use url::Url;

/// Accept pasted `example.com/path` by assuming HTTPS; everything else is left alone.
pub fn normalize(raw: &str) -> String {
    let raw = raw.trim();
    let host = raw.split(['/', '?', '#']).next().unwrap_or("");
    let looks_like_host = (host.contains('.') || host.starts_with("localhost"))
        && !host.contains(char::is_whitespace)
        && host.starts_with(|c: char| c.is_ascii_alphanumeric());
    // Local development servers rarely have TLS; everything else defaults to HTTPS.
    let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    let local = name == "localhost"
        || name
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !raw.contains("://") && !raw.starts_with("magnet:") && looks_like_host {
        format!("{}://{raw}", if local { "http" } else { "https" })
    } else {
        raw.to_owned()
    }
}

pub fn valid_url(raw: &str) -> Result<Url> {
    let url = Url::parse(&normalize(raw)).context("Expected an absolute HTTP(S) URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Native engine supports HTTP(S); use 'fluxget aria2' for other protocols"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Use -H Authorization:... instead of credentials embedded in URLs"
    );
    Ok(url)
}

pub fn client(options: &Network) -> Result<Client> {
    let mut headers = HeaderMap::new();
    for value in &options.headers {
        let (name, value) = value
            .split_once(':')
            .context("Headers require 'Name: value'")?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())?;
        ensure!(
            !matches!(
                name.as_str(),
                "range" | "if-range" | "if-match" | "accept-encoding" | "host" | "content-length"
            ),
            "Header {name} is managed by the download engine"
        );
        headers.insert(name, HeaderValue::from_str(value.trim())?);
    }
    headers.insert("accept-encoding", HeaderValue::from_static("identity"));
    let restrict_redirects = !options.headers.is_empty();
    let redirect = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("Too many redirects");
        }
        if !matches!(attempt.url().scheme(), "http" | "https") {
            return attempt.error("Non-HTTP redirect rejected");
        }
        if attempt
            .previous()
            .last()
            .is_some_and(|p| p.scheme() == "https" && attempt.url().scheme() == "http")
        {
            return attempt.error("HTTPS downgrade rejected");
        }
        if restrict_redirects
            && attempt
                .previous()
                .first()
                .is_some_and(|p| p.origin() != attempt.url().origin())
        {
            return attempt.error("Cross-origin redirect with custom headers: inspect the destination and request it explicitly");
        }
        attempt.follow()
    });
    let mut builder = Client::builder()
        .user_agent(concat!("fluxget/", env!("CARGO_PKG_VERSION")))
        .default_headers(headers)
        .redirect(redirect)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(options.timeout))
        .pool_max_idle_per_host(32);
    if let Some(proxy) = &options.proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct Info {
    pub url: String,
    pub size: Option<u64>,
    pub ranges: bool,
    pub etag: Option<String>,
    pub filename: String,
    pub content_type: String,
    pub protocol: String,
}

pub fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name)?.to_str().ok()
}

pub fn range(value: &str) -> Result<(u64, u64, u64)> {
    let (span, total) = value
        .strip_prefix("bytes ")
        .context("Invalid Content-Range unit")?
        .split_once('/')
        .context("Invalid Content-Range")?;
    let (start, end) = span.split_once('-').context("Invalid Content-Range span")?;
    let (start, end, total) = (
        start.parse::<u64>()?,
        end.parse::<u64>()?,
        total.parse::<u64>()?,
    );
    ensure!(start <= end && end < total, "Invalid Content-Range bounds");
    Ok((start, end, total))
}

pub fn identity(response: &Response) -> Result<()> {
    ensure!(
        header(response, "content-encoding").is_none_or(|v| v.eq_ignore_ascii_case("identity")),
        "Server ignored Accept-Encoding: identity"
    );
    Ok(())
}

pub fn retry_delay(response: &Response, attempt: u8) -> Duration {
    if let Some(value) = header(response, "retry-after") {
        if let Ok(seconds) = value.parse::<u64>() {
            return Duration::from_secs(seconds);
        }
        if let Ok(date) = httpdate::parse_http_date(value) {
            return date.duration_since(SystemTime::now()).unwrap_or_default();
        }
    }
    Duration::from_millis(250 * (1u64 << attempt.min(8)))
}

pub async fn request(builder: reqwest::RequestBuilder, retries: u8) -> Result<(Response, bool)> {
    let mut throttled = false;
    for attempt in 0..=retries {
        let result = builder
            .try_clone()
            .context("Request is not replayable")?
            .send()
            .await;
        match result {
            Ok(response) => {
                let status = response.status();
                if status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::SERVICE_UNAVAILABLE
                    || status == StatusCode::BAD_GATEWAY
                    || status == StatusCode::GATEWAY_TIMEOUT
                {
                    throttled |= status == StatusCode::TOO_MANY_REQUESTS
                        || status == StatusCode::SERVICE_UNAVAILABLE;
                    if attempt < retries {
                        let delay = retry_delay(&response, attempt);
                        drop(response);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                }
                if status == StatusCode::RANGE_NOT_SATISFIABLE {
                    return Ok((response, throttled));
                }
                return Ok((response.error_for_status()?, throttled));
            }
            Err(e) if attempt == retries => return Err(e.into()),
            Err(_) => tokio::time::sleep(Duration::from_millis(250 * (1u64 << attempt))).await,
        }
    }
    bail!("Retry budget exhausted")
}

pub fn html_type(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("text/html") || value.contains("application/xhtml+xml")
}

pub const PAGE_LIMIT: usize = 2 * 1024 * 1024;

pub enum Fetched {
    File(Info),
    Page { url: Url, body: String },
}

/// One request that serves as both range probe and landing-page fetch: HTML
/// servers ignore the range and return the page, files answer with one byte.
pub async fn fetch(client: &Client, url: Url, retries: u8) -> Result<Fetched> {
    let (response, _) = request(client.get(url).header("range", "bytes=0-0"), retries).await?;
    if !html_type(header(&response, "content-type").unwrap_or("")) {
        return Ok(Fetched::File(probe_response(response).await?));
    }
    let mut response = if response.status() == StatusCode::PARTIAL_CONTENT {
        // A static HTML file honored the range; fetch the whole document.
        let url = response.url().clone();
        drop(response);
        request(client.get(url), retries).await?.0
    } else {
        response
    };
    let url = response.url().clone();
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            data.len() + chunk.len() <= PAGE_LIMIT,
            "HTML page exceeds 2 MiB resolver limit; use --direct to save it"
        );
        data.extend_from_slice(&chunk);
    }
    Ok(Fetched::Page {
        url,
        body: String::from_utf8_lossy(&data).into_owned(),
    })
}

pub async fn probe(client: &Client, url: Url, retries: u8) -> Result<Info> {
    // A GET range probe establishes real support; Accept-Ranges and HEAD can lie.
    let (response, _) = request(client.get(url).header("range", "bytes=0-0"), retries).await?;
    probe_response(response).await
}

async fn probe_response(mut response: Response) -> Result<Info> {
    if response.status() == StatusCode::RANGE_NOT_SATISFIABLE
        && header(&response, "content-range") == Some("bytes */0")
    {
        return info(&response, Some(0), false);
    }
    response.error_for_status_ref()?;
    identity(&response)?;
    if response.status() == StatusCode::PARTIAL_CONTENT {
        let (start, end, total) =
            range(header(&response, "content-range").context("Missing Content-Range")?)?;
        ensure!(
            start == 0 && end == 0,
            "Server returned incorrect range for probe"
        );
        let result = info(&response, Some(total), true)?;
        let mut received = 0;
        while let Some(chunk) = response.chunk().await? {
            received += chunk.len();
            ensure!(received <= 1, "Range probe body too large");
        }
        ensure!(received == 1, "Truncated range probe");
        return Ok(result);
    }
    ensure!(
        response.status() == StatusCode::OK,
        "Unexpected probe status {}",
        response.status()
    );
    info(&response, response.content_length(), false)
}

fn info(response: &Response, size: Option<u64>, ranges: bool) -> Result<Info> {
    let etag = header(response, "etag")
        .filter(|v| v.starts_with('"') && v.ends_with('"'))
        .map(str::to_owned);
    let path_name = response
        .url()
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or("download.bin");
    let disposition = header(response, "content-disposition").and_then(|d| {
        d.split(';').find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            key.eq_ignore_ascii_case("filename")
                .then(|| value.trim_matches('"'))
        })
    });
    Ok(Info {
        url: response.url().as_str().to_owned(),
        size,
        ranges,
        etag,
        filename: safe_name(disposition.unwrap_or(path_name)),
        content_type: header(response, "content-type").unwrap_or("").to_owned(),
        protocol: format!("{:?}", response.version()),
    })
}

pub fn safe_name(raw: &str) -> String {
    let name: String = raw
        .chars()
        .take(160)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim_matches(['.', ' ']);
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if name.is_empty() {
        "download.bin".into()
    } else if reserved.contains(&stem.as_str()) {
        format!("_{name}")
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_validation() {
        assert_eq!(range("bytes 0-9/10").unwrap(), (0, 9, 10));
        for bad in [
            "bytes 9-0/10",
            "bytes 0-10/10",
            "bytes 0-1/*",
            "items 0-1/10",
            "bytes 0-18446744073709551616/20",
        ] {
            assert!(range(bad).is_err());
        }
    }
    #[test]
    fn pasted_hosts_get_a_scheme() {
        assert_eq!(normalize("github.com/o/r"), "https://github.com/o/r");
        assert_eq!(normalize("127.0.0.1:8080/x"), "http://127.0.0.1:8080/x");
        assert_eq!(normalize("localhost:3000"), "http://localhost:3000");
        assert_eq!(normalize("https://a.example/x"), "https://a.example/x");
        assert_eq!(
            normalize("magnet:?xt=urn:btih:abc"),
            "magnet:?xt=urn:btih:abc"
        );
        assert_eq!(normalize("--help"), "--help");
        assert_eq!(normalize("resolve"), "resolve");
    }
    #[test]
    fn portable_filenames() {
        assert_eq!(safe_name("../../a.exe"), "_.._a.exe");
        assert_eq!(safe_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_name("..."), "download.bin");
        assert!(!safe_name("a\nb\\c").contains(['\n', '\\']));
    }
}
