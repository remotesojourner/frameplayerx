//! Plex's seek previews: a BIF file for each video part, its pictures read
//! one at a time by byte range.

use super::{PlexSource, resource_url};
use crate::json::{array, text};
use crate::trickplay::{BIF_HEADER, bif_header, bif_index, bif_table_len};
use crate::urlutil::encode_form_value;
use crate::{Error, Result, Trickplay, TrickplayTiming};
use fp_core::source::{Entry, EntryKind};

impl PlexSource {
    pub(super) fn has_previews(&self, entry: &Entry) -> bool {
        entry.kind == EntryKind::Video
            && self
                .parse_location(&entry.location)
                .is_ok_and(|l| l.kind == "item")
    }

    pub(super) fn trickplay_for(&self, entry: &Entry) -> Result<Option<Trickplay>> {
        let location = self.parse_location(&entry.location)?;
        if location.kind != "item" {
            return Ok(None);
        }
        let raw = self.raw_item(&location.id)?;
        let media = self.choose_media(&raw, location.media.as_deref())?;
        // A video in several parts has a BIF file for each, which would
        // need placing after the parts before it.
        let [part] = array(media, "Part") else {
            return Ok(None);
        };
        // Plex marks a part whose previews it has made.
        if text(part, "indexes").as_deref() != Some("sd") {
            return Ok(None);
        }
        let Some(part_id) = text(part, "id") else {
            return Ok(None);
        };
        let endpoint = self.client()?;
        let path = format!("/library/parts/{}/indexes/sd", encode_form_value(&part_id));
        let url = resource_url(&endpoint.base, &path)?;
        let client = endpoint.client;
        let never = || false;
        let head = client.get_trickplay_range(&url, (0, BIF_HEADER), &never)?;
        let Some((count, multiplier)) = bif_header(&head) else {
            return Ok(None);
        };
        let table_end = BIF_HEADER + bif_table_len(count);
        let table = client.get_trickplay_range(&url, (BIF_HEADER, table_end), &never)?;
        let Some(index) = bif_index(&table, multiplier) else {
            return Ok(None);
        };
        let ranges = index.ranges;
        Ok(Some(Trickplay::new(
            TrickplayTiming::Listed(index.times_ms.into()),
            move |picture, cancelled| {
                let Some(&range) = ranges.get(picture as usize) else {
                    return Err(Error::NotFound("Plex preview picture".into()));
                };
                client.get_trickplay_range(&url, range, cancelled)
            },
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;
    use crate::config::PlexConfig;
    use crate::test_server::{TestServer, resp};
    use serde_json::json;

    fn metadata(indexes: Option<&str>) -> Vec<u8> {
        let mut part = json!({"id": 55, "file": "/films/Film.mkv"});
        if let Some(indexes) = indexes {
            part["indexes"] = json!(indexes);
        }
        serde_json::to_vec(&json!({"MediaContainer": {"Metadata": [{
            "ratingKey": "100", "type": "movie", "title": "Film",
            "Media": [{"id": 7, "height": 2160, "Part": [part]}]
        }]}}))
        .unwrap()
    }

    fn plex(server: &TestServer) -> PlexSource {
        PlexSource::new(&PlexConfig {
            id: "source".into(),
            name: "Plex".into(),
            url: server.base.clone(),
            machine_identifier: "machine".into(),
            access_token: "token".into(),
            connections: Vec::new(),
            auth: None,
            insecure_tls: false,
            parallel_downloads: false,
            max_height: None,
            local_only: false,
            allow_relay: false,
        })
        .unwrap()
    }

    fn serve(indexes: Option<&'static str>) -> TestServer {
        let file = crate::trickplay::tests::bif(&[b"first", b"second", b"third"], 2);
        TestServer::start(move |r| {
            let json = |body: Vec<u8>| resp(200, body, &[("Content-Type", "application/json")]);
            match r.path() {
                "/identity" => json(
                    serde_json::to_vec(&json!({"MediaContainer":{"machineIdentifier":"machine"}}))
                        .unwrap(),
                ),
                "/" => json(serde_json::to_vec(&json!({"MediaContainer":{}})).unwrap()),
                "/library/metadata/100" => json(metadata(indexes)),
                "/library/parts/55/indexes/sd" => {
                    assert_eq!(r.header("X-Plex-Token"), Some("token"));
                    let range = r.header("Range").expect("only parts of the file");
                    let (start, end) = range
                        .strip_prefix("bytes=")
                        .and_then(|r| r.split_once('-'))
                        .map(|(a, b)| (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()))
                        .unwrap();
                    let content_range = format!("bytes {start}-{end}/{}", file.len());
                    resp(
                        206,
                        file[start..=end].to_vec(),
                        &[("Content-Range", content_range.as_str())],
                    )
                }
                _ => resp(404, Vec::new(), &[]),
            }
        })
    }

    fn film() -> Entry {
        Entry::new(
            "Film",
            "plex://source/source/machine/item/100",
            EntryKind::Video,
        )
    }

    #[test]
    fn a_part_with_previews_gives_each_picture_by_its_moment() {
        let server = serve(Some("sd"));
        let source = plex(&server);
        assert!(source.has_trickplay(&film()));
        let preview = source.trickplay(&film()).unwrap().unwrap();
        assert_eq!(
            preview.timing,
            TrickplayTiming::Listed(vec![0, 2_000, 4_000].into())
        );
        assert_eq!(preview.fetch_sheet(1, &|| false).unwrap(), b"second");
        assert_eq!(preview.fetch_sheet(2, &|| false).unwrap(), b"third");
        assert!(preview.fetch_sheet(3, &|| false).is_err());
    }

    #[test]
    fn a_part_plex_made_no_previews_for_asks_for_none() {
        let server = serve(None);
        let source = plex(&server);
        assert!(source.trickplay(&film()).unwrap().is_none());
        assert_eq!(server.count(|r| r.path().starts_with("/library/parts/")), 0);
    }
}
