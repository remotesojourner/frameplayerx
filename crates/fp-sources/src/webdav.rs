//! WebDAV shares (Nextcloud, ownCloud, Synology/QNAP WebDAV, Apache
//! `mod_dav`, nginx-dav, rclone serve webdav, ...).
//!
//! Folders are listed with `PROPFIND` and `Depth: 1`; files are read with
//! the HTTP [`crate::http::HttpFile`] (GET with Range). Configured URLs may
//! use `webdav(s)://` or `dav(s)://`, which map to `http(s)://`. Locations
//! are source-owned relative identities, resolved against the current root.

use crate::config::{HttpConfig, SourceKind};
use crate::error::{Error, Result};
use crate::http::{HttpClient, HttpOptions};
use crate::httpdir::clean_url_and_credentials;
use crate::location;
use crate::timeutil::{parse_http_date, parse_iso8601};
use crate::urlutil::{
    is_http_url, parent_url, percent_decode, redact, resolve, webdav_to_http, with_trailing_slash,
};
use crate::xml;
use crate::{Source, dir_entry, file_entry, sort_entries};
use fp_core::source::{ByteSource, Entry};
use std::sync::Arc;
use std::time::Instant;

/// Properties requested for each child.
const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop>
<D:resourcetype/><D:getcontentlength/><D:getlastmodified/><D:displayname/><D:getcontenttype/>
</D:prop></D:propfind>"#;

/// A WebDAV share.
pub struct WebDavSource {
    id: String,
    name: String,
    root: String,
    client: HttpClient,
}

impl std::fmt::Debug for WebDavSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDavSource")
            .field("id", &self.id)
            .field("root", &redact(&self.root))
            .field("client", &self.client)
            .finish()
    }
}

impl WebDavSource {
    /// Creates the source. Fails when the URL is not `http(s)://`,
    /// `webdav(s)://` or `dav(s)://`.
    pub fn new(config: &HttpConfig) -> Result<WebDavSource> {
        let url = webdav_to_http(&config.url);
        if !is_http_url(&url) {
            return Err(Error::invalid(
                &config.url,
                "expected an http(s)://, webdav(s):// or dav(s):// URL",
            ));
        }
        let (url, creds) = clean_url_and_credentials(&url, config.credentials.as_ref())?;
        let opts = HttpOptions {
            insecure_tls: config.insecure_tls,
            cache: crate::cache::CacheOptions::for_downloads(config.parallel_downloads),
            ..HttpOptions::default()
        };
        Ok(WebDavSource {
            id: config.id.clone(),
            name: config.name.clone(),
            root: with_trailing_slash(&url),
            client: HttpClient::new(opts, creds.as_ref()).scoped_to(&url),
        })
    }

    /// URL listed by `list(None)`.
    pub fn root(&self) -> &str {
        &self.root
    }

    fn current_url(&self, value: &str) -> Result<String> {
        if crate::is_source_location(value) {
            let value = location::decode(value)
                .filter(|l| l.source_id == self.id && l.kind == "webdav")
                .ok_or_else(|| Error::invalid(value, "not owned by this WebDAV source"))?;
            return location::resolve_url(&self.root, &value.key)
                .ok_or_else(|| Error::invalid("WebDAV source resource", "invalid relative path"));
        }
        Ok(webdav_to_http(value))
    }

    /// Whether the server says `url` is a folder. An answer that doesn't say
    /// what it is fails: DELETE would take a folder whole.
    fn is_folder(&self, url: &str) -> Result<bool> {
        let reply = self
            .client
            .request(
                "PROPFIND",
                url,
                &[
                    ("Depth", "0"),
                    ("Content-Type", "application/xml; charset=utf-8"),
                ],
                Some(PROPFIND_BODY.as_bytes()),
                1 << 20,
            )?
            .error_for_status(url)?;
        let doc = xml::parse(&reply.text())?;
        let ms = doc
            .find("multistatus")
            .ok_or_else(|| Error::parse("WebDAV response", "no multistatus element"))?;
        ms.children_named("response")
            .flat_map(|r| r.children_named("propstat"))
            .filter(|ps| propstat_ok(ps))
            .find_map(|ps| ps.child("prop").and_then(|p| p.child("resourcetype")))
            .map(|rt| rt.child("collection").is_some())
            .ok_or_else(|| {
                Error::Unsupported(format!(
                    "{} didn't say whether this is a folder",
                    redact(url)
                ))
            })
    }
}

/// Whether a `propstat` holds properties the server could read: a 2xx
/// status, or none given.
fn propstat_ok(ps: &xml::Element) -> bool {
    ps.child_text("status").is_none_or(|s| {
        s.split_whitespace()
            .nth(1)
            .is_some_and(|c| c.starts_with('2'))
    })
}

/// Where an `href` in an answer for `request_url` leads, on the server that
/// answered: one behind a proxy can name another host, so only an HTTP
/// address's path counts, and any other scheme (`file:` too) is refused.
fn href_location(request_url: &str, href: &str) -> Option<String> {
    let joined = url::Url::parse(&resolve(request_url, href).ok()?).ok()?;
    if !matches!(joined.scheme(), "http" | "https") {
        return None;
    }
    let mut location = url::Url::parse(request_url).ok()?;
    location.set_path(joined.path());
    location.set_query(joined.query());
    Some(location.to_string())
}

impl Source for WebDavSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> SourceKind {
        SourceKind::WebDav
    }

    fn describe(&self) -> String {
        format!("WebDAV {}", redact(&self.root))
    }

    fn canonical_location(&self, value: &str) -> Option<String> {
        let url = self.current_url(value).ok()?;
        location::relative_url(&self.root, &url)
            .map(|key| location::encode(&self.id, "webdav", &key))
    }

    /// The folder's or file's address, with names as they are on the server.
    fn display_location(&self, location: Option<&str>) -> Option<String> {
        let location = location.unwrap_or(&self.root);
        self.canonical_location(location)?;
        Some(crate::urlutil::display_url(
            &self.current_url(location).ok()?,
        ))
    }

    fn list(&self, location: Option<&str>) -> Result<Vec<Entry>> {
        let url = with_trailing_slash(&self.current_url(location.unwrap_or(&self.root))?);
        let reply = self
            .client
            .request(
                "PROPFIND",
                &url,
                &[
                    ("Depth", "1"),
                    ("Content-Type", "application/xml; charset=utf-8"),
                ],
                Some(PROPFIND_BODY.as_bytes()),
                64 << 20,
            )?
            .error_for_status(&url)?;
        if reply.status != 207 && reply.status != 200 {
            return Err(Error::HttpStatus {
                status: reply.status,
                url: redact(&url),
            });
        }
        let mut entries = parse_multistatus(&reply.text(), &url)?;
        // What the server lists outside this source isn't its to offer.
        entries.retain_mut(|entry| match self.canonical_location(&entry.location) {
            Some(location) => {
                entry.location = location;
                true
            }
            None => false,
        });
        sort_entries(&mut entries);
        Ok(entries)
    }

    fn open(&self, location: &str) -> Result<Arc<dyn ByteSource>> {
        Ok(Arc::new(self.client.open(&self.current_url(location)?)?))
    }

    fn open_original(
        &self,
        video: &Entry,
        deadline: Instant,
    ) -> Result<Option<Arc<dyn ByteSource>>> {
        let Some(location) = self.canonical_location(&video.location) else {
            return Ok(None);
        };
        self.client
            .open_original(&self.current_url(&location)?, deadline)
    }

    fn picture_request(&self, location: &str) -> Option<(HttpClient, String)> {
        Some((self.client.clone(), self.current_url(location).ok()?))
    }

    fn parent(&self, location: &str) -> Option<String> {
        self.canonical_location(&parent_url(&self.current_url(location).ok()?)?)
    }

    fn sidecars(&self, video: &Entry) -> Result<crate::Sidecars> {
        let mut video = video.clone();
        video.location = self.current_url(&video.location)?;
        crate::sidecars_from_folder(self, &video)
    }

    fn can_delete(&self) -> bool {
        true
    }

    fn delete(&self, location: &str) -> Result<()> {
        let url = self.current_url(location)?;
        // DELETE takes a folder with everything in it, and a folder's address
        // needn't end in a slash, so the server is asked what it is first.
        if url.ends_with('/') || with_trailing_slash(&url) == self.root || self.is_folder(&url)? {
            return Err(Error::invalid(location, "a folder cannot be deleted"));
        }
        let reply = self
            .client
            .request_no_status_retry("DELETE", &url, &[], None, 64 << 10)?
            .error_for_status(&url)?;
        // A Multi-Status answer lists what couldn't be deleted.
        if reply.status == 207 {
            return Err(Error::HttpStatus {
                status: reply.status,
                url: redact(&url),
            });
        }
        Ok(())
    }
}

/// Path of a URL, decoded, without trailing slash, for comparing hrefs.
fn norm_path(url: &str) -> String {
    let path = url::Url::parse(url)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| url.to_string());
    percent_decode(path.trim_end_matches('/'))
}

/// Parses a `207 Multi-Status` PROPFIND answer for the folder `request_url`.
/// The folder itself is left out. Properties come from the `propstat` with a
/// 2xx status (or the only one when no status is given).
pub fn parse_multistatus(xml_text: &str, request_url: &str) -> Result<Vec<Entry>> {
    let doc = xml::parse(xml_text)?;
    let ms = doc
        .find("multistatus")
        .ok_or_else(|| Error::parse("WebDAV response", "no multistatus element"))?;
    let self_path = norm_path(request_url);
    let mut out = Vec::new();
    for resp in ms.children_named("response") {
        let Some(href) = resp.child_text("href") else {
            continue;
        };
        let Some(location) = href_location(request_url, href) else {
            log::debug!("skipping unusable WebDAV href {href:?}");
            continue;
        };
        if norm_path(&location) == self_path {
            continue;
        }
        let mut is_dir = false;
        let mut size = None;
        let mut modified = None;
        let mut display = None;
        for ps in resp.children_named("propstat") {
            if !propstat_ok(ps) {
                continue;
            }
            let Some(prop) = ps.child("prop") else {
                continue;
            };
            if let Some(rt) = prop.child("resourcetype") {
                is_dir |= rt.child("collection").is_some();
            }
            if let Some(len) = prop.child_text("getcontentlength") {
                size = len.parse::<u64>().ok();
            }
            if let Some(lm) = prop.child_text("getlastmodified") {
                modified = parse_http_date(lm).or_else(|| parse_iso8601(lm));
            }
            if let Some(dn) = prop.child_text("displayname") {
                display = Some(dn.to_string());
            }
        }
        // Some servers omit the trailing slash on collection hrefs.
        let location = if is_dir {
            with_trailing_slash(&location)
        } else {
            location
        };
        let mut name = crate::urlutil::last_segment(&location);
        if name.is_empty() {
            name = display.unwrap_or_default();
        }
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        out.push(if is_dir {
            dir_entry(name, location, modified)
        } else {
            file_entry(name, location, size, modified)
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::source::EntryKind;

    #[test]
    fn display_location_names_folders_and_files_on_the_server() {
        let s = WebDavSource::new(&HttpConfig {
            id: "w".into(),
            name: "W".into(),
            url: "https://bob:hunter2@nas.local/dav".into(),
            credentials: None,
            insecure_tls: false,
            parallel_downloads: false,
        })
        .unwrap();
        assert_eq!(
            s.display_location(None).as_deref(),
            Some("https://nas.local/dav")
        );
        let folder = s
            .canonical_location("https://nas.local/dav/My%20Films/")
            .unwrap();
        assert_eq!(
            s.display_location(Some(&folder)).as_deref(),
            Some("https://nas.local/dav/My Films")
        );
        let file = s
            .canonical_location("https://nas.local/dav/My%20Films/a%C3%A9.mp4")
            .unwrap();
        assert_eq!(
            s.display_location(Some(&file)).as_deref(),
            Some("https://nas.local/dav/My Films/aé.mp4")
        );
        let other = location::encode("other", "webdav", "a.mp4");
        assert_eq!(s.display_location(Some(&other)), None);
    }

    #[test]
    fn the_parallel_downloads_switch_reaches_the_client() {
        let source = |parallel_downloads| {
            WebDavSource::new(&HttpConfig {
                id: "w".into(),
                name: "W".into(),
                url: "https://example.invalid/dav/".into(),
                credentials: None,
                insecure_tls: false,
                parallel_downloads,
            })
            .unwrap()
        };
        assert_eq!(source(false).client.options().cache.parallel_requests, 1);
        assert!(source(true).client.options().cache.parallel_requests > 1);
    }

    #[test]
    fn parses_nextcloud_style_multistatus() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:oc="http://owncloud.org/ns">
 <d:response><d:href>/remote.php/dav/files/bob/VR/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype>
   <d:getlastmodified>Fri, 12 Jan 2024 10:22:33 GMT</d:getlastmodified></d:prop>
   <d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:getcontentlength/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/files/bob/VR/Sub%20Folder/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop>
   <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/files/bob/VR/Clip%20%231_MKX200.mp4</d:href>
  <d:propstat><d:prop><d:resourcetype/><d:getcontentlength>123456789</d:getcontentlength>
   <d:getlastmodified>Fri, 12 Jan 2024 10:22:33 GMT</d:getlastmodified></d:prop>
   <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>http://nas/remote.php/dav/files/bob/VR/%C3%BC.funscript</d:href>
  <d:propstat><d:prop><d:getcontentlength>10</d:getcontentlength></d:prop></d:propstat></d:response>
</d:multistatus>"#;
        let v = parse_multistatus(xml, "http://nas/remote.php/dav/files/bob/VR/").unwrap();
        assert_eq!(v.len(), 3, "{v:?}");
        assert_eq!(v[0].name, "Sub Folder");
        assert_eq!(v[0].kind, EntryKind::Directory);
        assert_eq!(
            v[0].location,
            "http://nas/remote.php/dav/files/bob/VR/Sub%20Folder/"
        );
        assert_eq!(v[1].name, "Clip #1_MKX200.mp4");
        assert_eq!(v[1].kind, EntryKind::Video);
        assert_eq!(v[1].size, Some(123_456_789));
        assert_eq!(v[1].modified, Some(1_705_054_953));
        assert_eq!(
            v[1].format.map(|f| f.projection),
            Some(fp_core::format::Projection::fisheye(200.0))
        );
        assert_eq!(
            v[1].location,
            "http://nas/remote.php/dav/files/bob/VR/Clip%20%231_MKX200.mp4"
        );
        assert_eq!(v[2].name, "ü.funscript");
        assert_eq!(v[2].size, Some(10));
    }

    #[test]
    fn collection_without_trailing_slash_and_errors() {
        let xml = r#"<multistatus xmlns="DAV:"><response><href>/dav/a</href>
<propstat><prop><resourcetype><collection/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat>
</response><response><href>/dav</href><propstat><prop><resourcetype><collection/></resourcetype></prop></propstat></response></multistatus>"#;
        let v = parse_multistatus(xml, "https://h/dav/").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].location, "https://h/dav/a/");
        assert!(parse_multistatus("<html/>", "https://h/").is_err());
    }

    #[test]
    fn accepts_dav_schemes() {
        let s = WebDavSource::new(&HttpConfig {
            id: "w".into(),
            name: "W".into(),
            url: "davs://u:secret@nas:5006/vr".into(),
            credentials: None,
            insecure_tls: true,
            parallel_downloads: false,
        })
        .unwrap();
        assert_eq!(s.root(), "https://nas:5006/vr/");
        assert!(!s.describe().contains("secret"));
        assert!(!format!("{s:?}").contains("secret"));
    }

    #[test]
    fn deletes_files_over_http_and_refuses_collections() {
        use crate::test_server::{TestServer, resp};

        let status = |kind: &str| {
            format!(
                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/x</d:href>
<d:propstat><d:prop><d:resourcetype>{kind}</d:resourcetype></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
            )
        };
        let server = TestServer::start(move |r| match (r.method.as_str(), r.path()) {
            ("PROPFIND", "/dav/Clip%201.mp4" | "/dav/locked.mp4") => {
                assert_eq!(r.header("Depth"), Some("0"));
                resp(207, status(""), &[])
            }
            // A folder whose address has no slash at the end.
            ("PROPFIND", "/dav/Sub") => resp(207, status("<d:collection/>"), &[]),
            ("PROPFIND", "/dav/missing.mp4") => resp(404, Vec::new(), &[]),
            // Its type isn't readable: it may be a folder.
            ("PROPFIND", "/dav/unknown.mp4") => resp(
                207,
                status("").replace("HTTP/1.1 200 OK", "HTTP/1.1 403 Forbidden"),
                &[],
            ),
            // A file, which a Multi-Status answer then fails to delete.
            ("PROPFIND" | "DELETE", "/dav/partial.mp4") => resp(207, status(""), &[]),
            ("DELETE", "/dav/Clip%201.mp4") => resp(204, Vec::new(), &[]),
            ("DELETE", "/dav/locked.mp4") => resp(403, Vec::new(), &[]),
            _ => resp(500, Vec::new(), &[]),
        });
        let source = WebDavSource::new(&HttpConfig {
            id: "w".into(),
            name: "WebDAV".into(),
            url: format!("{}/dav/", server.base),
            credentials: None,
            insecure_tls: false,
            parallel_downloads: false,
        })
        .unwrap();
        assert!(source.can_delete());

        let file = location::encode("w", "webdav", "Clip%201.mp4");
        source.delete(&file).unwrap();

        let locked = location::encode("w", "webdav", "locked.mp4");
        assert!(matches!(source.delete(&locked), Err(Error::Auth(_))));

        let missing = location::encode("w", "webdav", "missing.mp4");
        assert!(matches!(source.delete(&missing), Err(Error::NotFound(_))));

        let unknown = location::encode("w", "webdav", "unknown.mp4");
        assert!(matches!(
            source.delete(&unknown),
            Err(Error::Unsupported(_))
        ));

        let partial = location::encode("w", "webdav", "partial.mp4");
        assert!(matches!(
            source.delete(&partial),
            Err(Error::HttpStatus { status: 207, .. })
        ));

        for folder in [
            location::encode("w", "webdav", "Sub/"),
            location::encode("w", "webdav", "Sub"),
            format!("{}/dav", server.base),
        ] {
            assert!(
                matches!(source.delete(&folder), Err(Error::InvalidLocation { .. })),
                "{folder}"
            );
        }
        assert_eq!(server.count(|r| r.method == "DELETE"), 3);
        assert_eq!(
            server.count(|r| r.method == "DELETE" && r.path() == "/dav/unknown.mp4"),
            0
        );
    }

    #[test]
    fn a_listing_offers_only_what_is_in_the_source() {
        use crate::test_server::{TestServer, resp};

        let server = TestServer::start(|r| {
            let entry = |href: &str| {
                format!(
                    "<d:response><d:href>{href}</d:href><d:propstat><d:prop>\
                     <d:resourcetype/><d:getcontentlength>5</d:getcontentlength></d:prop>\
                     <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
                )
            };
            let body = format!(
                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">{}{}{}{}{}</d:multistatus>"#,
                entry("/dav/"),
                entry("/dav/here.mp4"),
                // A server behind a proxy, naming its own address.
                entry("http://internal:8080/dav/proxied.mp4"),
                entry("file:///home/steamos/local.mp4"),
                entry("/elsewhere/outside.mp4"),
            );
            match (r.method.as_str(), r.path()) {
                ("PROPFIND", "/dav/") => resp(207, body, &[]),
                _ => resp(500, Vec::new(), &[]),
            }
        });
        let source = WebDavSource::new(&HttpConfig {
            id: "w".into(),
            name: "WebDAV".into(),
            url: format!("{}/dav/", server.base),
            credentials: None,
            insecure_tls: false,
            parallel_downloads: false,
        })
        .unwrap();
        let listed = source.list(None).unwrap();
        let names: Vec<_> = listed.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["here.mp4", "proxied.mp4"]);
        for e in &listed {
            assert!(
                crate::location_owner(&e.location).as_deref() == Some("w"),
                "{}",
                e.location
            );
        }
    }
}
