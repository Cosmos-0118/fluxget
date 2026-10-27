//! Public listing APIs of the sites people download from most: release
//! assets from GitHub, GitLab and Gitea/Forgejo, model files on Hugging Face,
//! archive.org items and SourceForge projects. Pages are never scripted; the
//! documented JSON APIs give exact file names, sizes and (where published)
//! SHA-256 digests, which the engine then verifies.
use crate::{
    http::{self, Fetched},
    resolver::{self, Candidate},
    sites::{self, enc, human, join},
};
use anyhow::Result;
use reqwest::{Client, StatusCode};
use serde_json::Value;
use url::Url;

#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    /// Repository front page: newest release, else a source archive.
    Repo,
    Latest,
    List,
    Tag(String),
}

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Forge {
    GitHub {
        owner: String,
        repo: String,
        scope: Scope,
    },
    GitLab {
        origin: String,
        project: String,
        scope: Scope,
    },
    Gitea {
        origin: String,
        owner: String,
        repo: String,
        scope: Scope,
    },
    HuggingFace {
        dataset: bool,
        repo: String,
        rev: String,
        path: String,
    },
    ArchiveOrg {
        id: String,
    },
    SourceForge {
        project: String,
    },
}

/// Candidates a forge page stands for, or `None` to treat the URL as an
/// ordinary web page (unknown repository, no API, no files).
pub async fn expand(client: &Client, forge: &Forge) -> Result<Option<Vec<Candidate>>> {
    let found = match forge {
        Forge::GitHub { owner, repo, scope } => github(client, owner, repo, scope).await?,
        Forge::GitLab {
            origin,
            project,
            scope,
        } => gitlab(client, origin, project, scope).await?,
        Forge::Gitea {
            origin,
            owner,
            repo,
            scope,
        } => gitea(client, origin, owner, repo, scope).await?,
        Forge::HuggingFace {
            dataset,
            repo,
            rev,
            path,
        } => hugging_face(client, *dataset, repo, rev, path).await?,
        Forge::ArchiveOrg { id } => archive_org(client, id).await?,
        Forge::SourceForge { project } => sourceforge(client, project).await?,
    };
    Ok(found.filter(|v| !v.is_empty()))
}

enum Json {
    Found(Value),
    /// Not found, or not an API answer (an ordinary web server's reply).
    Missing,
    Denied(StatusCode),
}

async fn get_json(client: &Client, url: &str, token: Option<String>) -> Result<Json> {
    let mut builder = client.get(url).header("accept", "application/json");
    if let Some(token) = token {
        builder = builder.bearer_auth(token);
    }
    match http::request(builder, 0).await {
        Ok((mut response, _)) => {
            let mut data = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                anyhow::ensure!(
                    data.len() + chunk.len() <= 32 << 20,
                    "API answer exceeds 32 MiB"
                );
                data.extend_from_slice(&chunk);
            }
            Ok(serde_json::from_slice(&data).map_or(Json::Missing, Json::Found))
        }
        Err(error) => {
            match error
                .downcast_ref::<reqwest::Error>()
                .and_then(|e| e.status())
            {
                Some(StatusCode::NOT_FOUND | StatusCode::GONE) => Ok(Json::Missing),
                Some(
                    status @ (StatusCode::UNAUTHORIZED
                    | StatusCode::FORBIDDEN
                    | StatusCode::TOO_MANY_REQUESTS),
                ) => Ok(Json::Denied(status)),
                _ => Err(error),
            }
        }
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn sha256_of(digest: &str) -> Option<String> {
    let hex = digest
        .strip_prefix("sha256:")
        .unwrap_or(digest)
        .to_ascii_lowercase();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

fn env_token(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|n| std::env::var(n).ok())
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

fn prefixed(prefix: &str, label: String) -> String {
    format!("{prefix}{label}")
}

// ---- GitHub -------------------------------------------------------------

pub fn github_release(
    release: &Value,
    multi: bool,
    web: &str,
    owner: &str,
    repo: &str,
) -> Vec<Candidate> {
    let tag = text(release, "tag_name");
    let mut prefix = if multi {
        format!("[{tag}] ")
    } else {
        String::new()
    };
    if release.get("prerelease").and_then(Value::as_bool) == Some(true) {
        prefix.push_str("(pre-release) ");
    }
    let mut out = Vec::new();
    for asset in release
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (name, url) = (text(asset, "name"), text(asset, "browser_download_url"));
        if name.is_empty()
            || url.is_empty()
            || asset
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|s| s != "uploaded")
        {
            continue;
        }
        let size = asset
            .get("size")
            .and_then(Value::as_u64)
            .map(|n| format!(" ({})", human(n)))
            .unwrap_or_default();
        out.push(
            Candidate::new(url, prefixed(&prefix, format!("{name}{size}")))
                .sha256(sha256_of(text(asset, "digest"))),
        );
    }
    if !tag.is_empty() {
        for (ext, name) in [(".zip", "zip"), (".tar.gz", "tar.gz")] {
            out.push(Candidate::new(
                join(web, &[owner, repo, "archive", "refs", "tags", tag]) + ext,
                prefixed(&prefix, format!("Source code ({name})")),
            ));
        }
    }
    out
}

async fn github_get(client: &Client, base: &str, path: &str) -> Result<Json> {
    get_json(
        client,
        &format!("{base}{path}"),
        env_token(&["GITHUB_TOKEN", "GH_TOKEN"]),
    )
    .await
}

async fn github(
    client: &Client,
    owner: &str,
    repo: &str,
    scope: &Scope,
) -> Result<Option<Vec<Candidate>>> {
    let gh = sites::github();
    let web = gh.web.as_str().trim_end_matches('/').to_owned();
    let base = join(&gh.api, &["repos", owner, repo]);
    let release = match scope {
        Scope::Tag(tag) => {
            github_get(client, &base, &format!("/releases/tags/{}", enc_tag(tag))).await?
        }
        Scope::List => github_get(client, &base, "/releases?per_page=5").await?,
        Scope::Latest | Scope::Repo => github_get(client, &base, "/releases/latest").await?,
    };
    match release {
        Json::Denied(status) => {
            github_page_fallback(client, &web, owner, repo, scope, status).await
        }
        Json::Found(Value::Array(list)) => Ok(Some(
            list.iter()
                .filter(|r| r.get("draft").and_then(Value::as_bool) != Some(true))
                .take(3)
                .flat_map(|r| github_release(r, true, &web, owner, repo))
                .collect(),
        )),
        Json::Found(release) => Ok(Some(github_release(&release, false, &web, owner, repo))),
        Json::Missing if matches!(scope, Scope::Latest | Scope::Repo) => {
            // No stable release: the newest release of any kind, else the source.
            match github_get(client, &base, "/releases?per_page=1").await? {
                Json::Found(Value::Array(list)) if !list.is_empty() => {
                    Ok(Some(github_release(&list[0], false, &web, owner, repo)))
                }
                Json::Found(_) if matches!(scope, Scope::Repo) => {
                    let Json::Found(info) = github_get(client, &base, "").await? else {
                        return Ok(None);
                    };
                    let branch = Some(text(&info, "default_branch"))
                        .filter(|b| !b.is_empty())
                        .unwrap_or("HEAD");
                    Ok(Some(
                        [(".zip", "zip"), (".tar.gz", "tar.gz")]
                            .iter()
                            .map(|(ext, name)| {
                                Candidate::new(
                                    join(&web, &[owner, repo, "archive", "refs", "heads", branch])
                                        + ext,
                                    format!("Source code ({name}) - {branch}"),
                                )
                            })
                            .collect(),
                    ))
                }
                Json::Denied(status) => {
                    github_page_fallback(client, &web, owner, repo, scope, status).await
                }
                _ => Ok(None),
            }
        }
        Json::Missing => Ok(None),
    }
}

fn enc_tag(tag: &str) -> String {
    tag.split('/').map(enc).collect::<Vec<_>>().join("/")
}

/// The unauthenticated API allows 60 requests an hour per address. The release
/// page's own asset fragment is not rate limited, but carries no checksums.
async fn github_page_fallback(
    client: &Client,
    web: &str,
    owner: &str,
    repo: &str,
    scope: &Scope,
    status: StatusCode,
) -> Result<Option<Vec<Candidate>>> {
    let blocked = format!(
        "GitHub API answered {status} (rate limit or no access). Set GITHUB_TOKEN to raise the limit"
    );
    let tag = match scope {
        Scope::Tag(tag) => tag.clone(),
        Scope::Latest | Scope::Repo => {
            let latest = Url::parse(&join(web, &[owner, repo, "releases", "latest"]))?;
            // GitHub redirects /releases/latest to /releases/tag/<tag>.
            let Ok(Fetched::Page { url, .. }) = http::fetch(client, latest, 1).await else {
                anyhow::bail!(blocked)
            };
            let parts: Vec<_> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
            match parts.iter().position(|p: &&str| *p == "tag") {
                Some(at) if at + 1 < parts.len() => parts[at + 1..]
                    .iter()
                    .map(|p| {
                        percent_encoding::percent_decode_str(p)
                            .decode_utf8_lossy()
                            .into_owned()
                    })
                    .collect::<Vec<_>>()
                    .join("/"),
                _ => return Ok(None),
            }
        }
        Scope::List => anyhow::bail!(blocked),
    };
    eprintln!("{blocked}; using the release page instead (no checksum verification).");
    let fragment = Url::parse(&join(
        web,
        &[owner, repo, "releases", "expanded_assets", &tag],
    ))?;
    let Fetched::Page { url, body } = http::fetch(client, fragment, 1).await? else {
        return Ok(None);
    };
    let page = resolver::extract(&body, &url);
    let mut out: Vec<Candidate> = page
        .links
        .into_iter()
        .filter(|c| c.url.contains("/releases/download/"))
        .collect();
    for (ext, name) in [(".zip", "zip"), (".tar.gz", "tar.gz")] {
        out.push(Candidate::new(
            join(web, &[owner, repo, "archive", "refs", "tags", &tag]) + ext,
            format!("Source code ({name})"),
        ));
    }
    Ok(Some(out))
}

// ---- GitLab -------------------------------------------------------------

pub fn gitlab_release(release: &Value, multi: bool) -> Vec<Candidate> {
    let tag = text(release, "tag_name");
    let prefix = if multi {
        format!("[{tag}] ")
    } else {
        String::new()
    };
    let mut out = Vec::new();
    let assets = release.get("assets");
    for link in assets
        .and_then(|a| a.get("links"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let url = Some(text(link, "direct_asset_url"))
            .filter(|u| !u.is_empty())
            .unwrap_or(text(link, "url"));
        if !url.is_empty() {
            out.push(Candidate::new(
                url,
                prefixed(&prefix, text(link, "name").to_owned()),
            ));
        }
    }
    for source in assets
        .and_then(|a| a.get("sources"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (url, format) = (text(source, "url"), text(source, "format"));
        if !url.is_empty() && matches!(format, "zip" | "tar.gz") {
            out.push(Candidate::new(
                url,
                prefixed(&prefix, format!("Source code ({format})")),
            ));
        }
    }
    out
}

async fn gitlab(
    client: &Client,
    origin: &str,
    project: &str,
    scope: &Scope,
) -> Result<Option<Vec<Candidate>>> {
    let api = format!(
        "{origin}/api/v4/projects/{}/releases",
        enc(project).replace('/', "%2F")
    );
    let token = env_token(&["GITLAB_TOKEN"]);
    let url = match scope {
        Scope::Tag(tag) => format!("{api}/{}", enc(tag)),
        Scope::List => format!("{api}?per_page=3"),
        Scope::Latest | Scope::Repo => format!("{api}?per_page=1"),
    };
    Ok(match get_json(client, &url, token).await? {
        Json::Found(Value::Array(list)) => {
            let multi = matches!(scope, Scope::List);
            Some(list.iter().flat_map(|r| gitlab_release(r, multi)).collect())
        }
        Json::Found(release @ Value::Object(_)) if release.get("tag_name").is_some() => {
            Some(gitlab_release(&release, false))
        }
        _ => None,
    })
}

// ---- Gitea / Forgejo / Codeberg ----------------------------------------------

pub fn gitea_release(release: &Value, multi: bool) -> Vec<Candidate> {
    let tag = text(release, "tag_name");
    let mut prefix = if multi {
        format!("[{tag}] ")
    } else {
        String::new()
    };
    if release.get("prerelease").and_then(Value::as_bool) == Some(true) {
        prefix.push_str("(pre-release) ");
    }
    let mut out = Vec::new();
    for asset in release
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (name, url) = (text(asset, "name"), text(asset, "browser_download_url"));
        if !name.is_empty() && !url.is_empty() {
            let size = asset
                .get("size")
                .and_then(Value::as_u64)
                .map(|n| format!(" ({})", human(n)))
                .unwrap_or_default();
            out.push(Candidate::new(
                url,
                prefixed(&prefix, format!("{name}{size}")),
            ));
        }
    }
    for (key, name) in [("zipball_url", "zip"), ("tarball_url", "tar.gz")] {
        let url = text(release, key);
        if !url.is_empty() {
            out.push(Candidate::new(
                url,
                prefixed(&prefix, format!("Source code ({name})")),
            ));
        }
    }
    out
}

async fn gitea(
    client: &Client,
    origin: &str,
    owner: &str,
    repo: &str,
    scope: &Scope,
) -> Result<Option<Vec<Candidate>>> {
    let api = format!(
        "{}/releases",
        join(&format!("{origin}/api/v1/repos"), &[owner, repo])
    );
    let token = env_token(&["GITEA_TOKEN", "CODEBERG_TOKEN"]);
    let url = match scope {
        Scope::Tag(tag) => format!("{api}/tags/{}", enc_tag(tag)),
        Scope::List => format!("{api}?limit=3"),
        Scope::Latest | Scope::Repo => format!("{api}/latest"),
    };
    Ok(match get_json(client, &url, token).await? {
        Json::Found(Value::Array(list)) => Some(
            list.iter()
                .filter(|r| r.get("draft").and_then(Value::as_bool) != Some(true))
                .flat_map(|r| gitea_release(r, true))
                .collect(),
        ),
        Json::Found(release @ Value::Object(_)) if release.get("tag_name").is_some() => {
            Some(gitea_release(&release, false))
        }
        _ => None,
    })
}

// ---- Hugging Face ----------------------------------------------------------

pub fn hugging_face_files(
    tree: &Value,
    dataset: bool,
    repo: &str,
    rev: &str,
    path: &str,
) -> Vec<Candidate> {
    let base = if dataset {
        join("https://huggingface.co", &["datasets", repo])
    } else {
        join("https://huggingface.co", &[repo])
    };
    let prefix = path.trim_matches('/');
    tree.as_array()
        .into_iter()
        .flatten()
        .filter(|entry| text(entry, "type") == "file")
        .filter_map(|entry| {
            let file = text(entry, "path");
            let inside = prefix.is_empty()
                || file
                    .strip_prefix(prefix)
                    .is_some_and(|r| r.starts_with('/'));
            if file.is_empty() || file == ".gitattributes" || !inside {
                return None;
            }
            let size = entry
                .get("size")
                .and_then(Value::as_u64)
                .map(|n| format!(" ({})", human(n)))
                .unwrap_or_default();
            // LFS objects are addressed by their SHA-256; git blobs use SHA-1.
            let digest = entry.get("lfs").map(|l| text(l, "oid")).and_then(sha256_of);
            Some(
                Candidate::new(
                    format!("{}?download=true", join(&base, &["resolve", rev, file])),
                    format!("{file}{size}"),
                )
                .sha256(digest),
            )
        })
        .collect()
}

async fn hugging_face(
    client: &Client,
    dataset: bool,
    repo: &str,
    rev: &str,
    path: &str,
) -> Result<Option<Vec<Candidate>>> {
    let kind = if dataset { "datasets" } else { "models" };
    let url = format!(
        "{}/tree/{}?recursive=true",
        join("https://huggingface.co/api", &[kind, repo]),
        enc(rev)
    );
    let token = env_token(&["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"]);
    match get_json(client, &url, token).await? {
        Json::Found(tree @ Value::Array(_)) => Ok(Some(hugging_face_files(&tree, dataset, repo, rev, path))),
        Json::Denied(status) => anyhow::bail!(
            "Hugging Face answered {status}: the repository is private or gated. Accept its terms in a browser and pass -H 'Authorization: Bearer TOKEN' (or set HF_TOKEN)"
        ),
        _ => Ok(None),
    }
}

// ---- archive.org -------------------------------------------------------------

pub fn archive_files(metadata: &Value, id: &str) -> Vec<Candidate> {
    let mut files: Vec<(bool, &Value)> = metadata
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|f| {
            let name = text(f, "name");
            let internal = [
                "_meta.xml",
                "_files.xml",
                "_meta.sqlite",
                "_reviews.xml",
                "__ia_thumb.jpg",
            ]
            .iter()
            .any(|suffix| name.ends_with(suffix));
            !name.is_empty() && text(f, "source") != "metadata" && !internal
        })
        .map(|f| (text(f, "source") == "original", f))
        .collect();
    files.sort_by_key(|(original, f)| (!*original, text(f, "name").to_owned()));
    files
        .into_iter()
        .map(|(_, f)| {
            let name = text(f, "name");
            let size = text(f, "size").parse::<u64>().ok().map(human);
            let detail = [
                Some(text(f, "format"))
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                size,
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
            let label = if detail.is_empty() {
                name.to_owned()
            } else {
                format!("{name} ({detail})")
            };
            Candidate::new(join("https://archive.org/download", &[id, name]), label)
        })
        .collect()
}

async fn archive_org(client: &Client, id: &str) -> Result<Option<Vec<Candidate>>> {
    let url = join("https://archive.org/metadata", &[id]);
    Ok(match get_json(client, &url, None).await? {
        Json::Found(metadata) => Some(archive_files(&metadata, id)),
        _ => None,
    })
}

// ---- SourceForge -----------------------------------------------------------

pub fn sourceforge_files(best: &Value) -> Vec<Candidate> {
    // One file is often the best release for several platforms; list it once.
    let mut files: Vec<(Vec<String>, &Value)> = Vec::new();
    let mut entries: Vec<(String, &Value)> = Vec::new();
    if let Some(map) = best.get("platform_releases").and_then(Value::as_object) {
        for (key, release) in map {
            let platform = match key.as_str() {
                "windows" => "Windows",
                "mac" => "macOS",
                "linux" => "Linux",
                "android" => "Android",
                other => other,
            };
            entries.push((platform.to_owned(), release));
        }
    }
    if let Some(release) = best.get("release") {
        entries.push(("Latest".into(), release));
    }
    for (platform, release) in entries {
        let url = text(release, "url");
        if url.is_empty() {
            continue;
        }
        match files
            .iter_mut()
            .find(|(_, known)| text(known, "url") == url)
        {
            Some((platforms, _)) => {
                if platform != "Latest" {
                    platforms.push(platform);
                }
            }
            None => files.push((vec![platform], release)),
        }
    }
    files
        .into_iter()
        .map(|(platforms, release)| {
            let file = text(release, "filename").rsplit('/').next().unwrap_or("");
            let size = release
                .get("bytes")
                .and_then(Value::as_u64)
                .map(|n| format!(" ({})", human(n)))
                .unwrap_or_default();
            Candidate::new(
                text(release, "url"),
                format!("{}: {file}{size}", platforms.join("/")),
            )
            .sha256(sha256_of(text(release, "sha256sum")))
        })
        .collect()
}

async fn sourceforge(client: &Client, project: &str) -> Result<Option<Vec<Candidate>>> {
    let url = join(
        "https://sourceforge.net/projects",
        &[project, "best_release.json"],
    );
    Ok(match get_json(client, &url, None).await? {
        Json::Found(best) => Some(sourceforge_files(&best)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HASH: &str = "3750b2e93f37e0c692657da574d7019a101c0084da05a790c83fd335bad973e4";

    #[test]
    fn github_release_lists_assets_digests_and_sources() {
        let release = json!({
            "tag_name": "v1.2/beta", "prerelease": true,
            "assets": [
                {"name": "tool-windows-x64.zip", "size": 1536, "state": "uploaded",
                 "browser_download_url": "https://github.com/o/r/releases/download/v1.2%2Fbeta/tool-windows-x64.zip",
                 "digest": format!("sha256:{HASH}")},
                {"name": "broken.zip", "state": "starter", "browser_download_url": "https://x/y"},
                {"name": "nodigest.tar.gz", "browser_download_url": "https://github.com/o/r/releases/download/t/nodigest.tar.gz", "digest": "sha1:abc"}
            ]
        });
        let c = github_release(&release, true, "https://github.com", "o", "r");
        assert_eq!(c.len(), 4);
        assert_eq!(
            c[0].label,
            "[v1.2/beta] (pre-release) tool-windows-x64.zip (1.5 KiB)"
        );
        assert_eq!(c[0].sha256.as_deref(), Some(HASH));
        assert!(c[1].sha256.is_none());
        assert_eq!(
            c[2].url,
            "https://github.com/o/r/archive/refs/tags/v1.2/beta.zip"
        );
        assert!(c[3].url.ends_with("/beta.tar.gz"));
    }

    #[test]
    fn gitlab_and_gitea_releases() {
        let gitlab = json!({"tag_name": "v3", "assets": {
            "links": [{"name": "Installer", "url": "https://gl/u", "direct_asset_url": "https://gl/d/installer.exe"}],
            "sources": [{"format": "zip", "url": "https://gl/s.zip"}, {"format": "tar.bz2", "url": "https://gl/s.bz2"}]}});
        let c = gitlab_release(&gitlab, false);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].url, "https://gl/d/installer.exe");
        let gitea = json!({"tag_name": "v9", "assets": [{"name": "a.deb", "size": 10, "browser_download_url": "https://cb/a.deb"}],
            "zipball_url": "https://cb/z.zip", "tarball_url": "https://cb/t.tar.gz"});
        let c = gitea_release(&gitea, true);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].label, "[v9] a.deb (10 B)");
    }

    #[test]
    fn hugging_face_files_carry_lfs_sha256() {
        let tree = json!([
            {"type": "file", "path": ".gitattributes", "size": 3},
            {"type": "directory", "path": "onnx"},
            {"type": "file", "path": "config.json", "size": 285, "oid": "27df7aef8d7989e557b54552c0690e7fa1b2bc8c"},
            {"type": "file", "path": "onnx/model q4.gguf", "size": 2048, "oid": "ab", "lfs": {"oid": HASH, "size": 2048}}
        ]);
        let all = hugging_face_files(&tree, false, "o/m", "main", "");
        assert_eq!(all.len(), 2);
        assert!(all[0].sha256.is_none());
        assert_eq!(all[1].sha256.as_deref(), Some(HASH));
        assert_eq!(
            all[1].url,
            "https://huggingface.co/o/m/resolve/main/onnx/model%20q4.gguf?download=true"
        );
        let sub = hugging_face_files(&tree, true, "o/d", "main", "onnx");
        assert_eq!(sub.len(), 1);
        assert!(sub[0]
            .url
            .starts_with("https://huggingface.co/datasets/o/d/resolve/"));
    }

    #[test]
    fn archive_org_skips_internal_files_and_prefers_originals() {
        let meta = json!({"files": [
            {"name": "item_meta.xml", "source": "original", "format": "Metadata"},
            {"name": "b.mp3", "source": "derivative", "format": "VBR MP3", "size": "2048"},
            {"name": "a dir/a.flac", "source": "original", "format": "Flac", "size": "1048576"},
            {"name": "item_archive.torrent", "source": "metadata"}
        ]});
        let c = archive_files(&meta, "item");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].url, "https://archive.org/download/item/a%20dir/a.flac");
        assert_eq!(c[0].label, "a dir/a.flac (Flac, 1.0 MiB)");
        assert!(archive_files(&json!({}), "missing").is_empty());
    }

    #[test]
    fn sourceforge_best_release_per_platform() {
        let exe = json!({"url": "https://sourceforge.net/projects/p/files/1/p.exe/download", "filename": "/1/p.exe", "bytes": 1048576, "sha256sum": HASH});
        let best = json!({
            "platform_releases": {
                "windows": exe.clone(),
                "linux": {"url": "https://sourceforge.net/projects/p/files/1/p.tar.xz/download", "filename": "/1/p.tar.xz"},
                "mac": exe
            },
            "release": {"url": "https://sourceforge.net/projects/p/files/1/p.tar.xz/download", "filename": "/1/p.tar.xz"}
        });
        let c = sourceforge_files(&best);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].label, "Linux: p.tar.xz");
        assert_eq!(c[1].label, "macOS/Windows: p.exe (1.0 MiB)");
        assert_eq!(c[1].sha256.as_deref(), Some(HASH));
    }
}
