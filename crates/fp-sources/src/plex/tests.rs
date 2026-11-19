//! Renewal across the account and server boundaries, without live credentials.

use super::*;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SystemRandom;
use ring::signature::Ed25519KeyPair;
use serde_json::json;

use crate::test_server::{TestServer, resp};

#[test]
fn rejected_saved_credentials_renew_at_handshake_or_on_a_later_request() {
    for reject_at_handshake in [true, false] {
        let server = TestServer::start(move |r| {
            let token = r.header("X-Plex-Token");
            let body = match r.path() {
                "/identity" => json!({"MediaContainer":{"machineIdentifier":"machine"}}),
                "/" | "/library/sections" if token == Some("server-old") && reject_at_handshake => {
                    return resp(401, Vec::new(), &[]);
                }
                "/" => json!({"MediaContainer":{}}),
                "/library/sections" => json!({"MediaContainer":{"Directory":[]}}),
                "/library/metadata/100" if token == Some("server-new") => {
                    json!({"MediaContainer":{"Metadata":[{"ratingKey":"100","type":"movie","title":"Film"}]}})
                }
                _ => return resp(401, Vec::new(), &[]),
            };
            resp(
                200,
                serde_json::to_vec(&body).unwrap(),
                &[("Content-Type", "application/json")],
            )
        });
        let base = server.base.clone();
        let account = TestServer::start(move |r| {
            let body = match r.path() {
                "/auth/nonce" => json!({"nonce":"nonce"}),
                "/auth/token" => json!({"auth_token":"account-new"}),
                "/resources" => {
                    assert_eq!(r.header("X-Plex-Token"), Some("account-new"));
                    json!([{"name":"Plex","provides":"server","clientIdentifier":"machine","accessToken":"server-new",
                        "connections":[{"uri":base,"local":true,"relay":false}]}])
                }
                _ => return resp(404, Vec::new(), &[]),
            };
            resp(
                200,
                serde_json::to_vec(&body).unwrap(),
                &[("Content-Type", "application/json")],
            )
        });
        let api = PlexAuthClient::with_test_endpoint(&account.base).unwrap();
        let private = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let auth = serde_json::from_value::<auth::PlexCredentials>(json!({
            "access_token":"account-old", "client_identifier":"saved-client",
            "device_key":{"id":"saved-key", "pkcs8":URL_SAFE_NO_PAD.encode(private.as_ref())},
            "expires_at":4_000_000_000_u64,
        }))
        .unwrap();
        let config = PlexConfig {
            id: "source".into(),
            name: "Plex".into(),
            url: server.base.clone(),
            machine_identifier: "machine".into(),
            access_token: "server-old".into(),
            connections: Vec::new(),
            auth: Some(auth),
            insecure_tls: false,
            parallel_downloads: false,
            max_height: None,
            local_only: false,
            allow_relay: true,
        };
        let mut source = PlexSource::new(&config).unwrap();
        source.auth_api = api;
        source.probe().unwrap();
        assert_eq!(source.item("100").unwrap().name, "Film");
        assert_eq!(account.count(|r| r.path() == "/auth/token"), 1);
        let Some(SourceConfig::Plex(updated)) = source.updated_config() else {
            panic!("renewed credentials must be persisted");
        };
        assert_eq!(updated.id, config.id);
        assert_eq!(updated.access_token, "server-new");
        assert_eq!(updated.auth.unwrap().access_token, "account-new");
        assert!(source.updated_config().is_none());
        assert!(
            server
                .requests()
                .iter()
                .all(|r| r.header("X-Plex-Token") != Some("account-new"))
        );
    }
}

#[test]
fn a_listed_remote_address_has_to_be_encrypted() {
    let listed = |uri: &str| PlexConnection {
        uri: uri.into(),
        address: None,
        port: None,
        local: false,
        relay: false,
        protocol: uri.split(':').next().unwrap().into(),
    };
    let config = |url: &str, connections: Vec<PlexConnection>| PlexConfig {
        id: "source".into(),
        name: "Plex".into(),
        url: url.into(),
        machine_identifier: "machine".into(),
        access_token: "secret".into(),
        connections,
        auth: None,
        insecure_tls: false,
        parallel_downloads: false,
        max_height: None,
        local_only: false,
        allow_relay: false,
    };
    let tried =
        |c: &PlexConfig| -> Vec<String> { connections(c).into_iter().map(|c| c.uri).collect() };
    let https = "https://1-2-3-4.abc.plex.direct:32400";
    for (plain, kept) in [
        ("http://203.0.113.5:32400", false),
        ("http://[2001:db8::5]:32400", false),
        ("http://plex.example.com:32400", false),
        ("http://192.168.1.5:32400", true),
        ("http://[fd00::5]:32400", true),
        ("http://nas.local:32400", true),
        ("http://nas:32400", true),
    ] {
        let c = config(https, vec![listed(https), listed(plain)]);
        assert_eq!(tried(&c).contains(&plain.to_string()), kept, "{plain}");
        assert!(tried(&c).contains(&https.to_string()));
    }
    // A saved address is held to the same rule on a linked source.
    let plain = "http://203.0.113.5:32400";
    let c = config(plain, vec![listed(https), listed(plain)]);
    assert_eq!(tried(&c), [https]);
    let c = config(plain, vec![listed(https)]);
    assert_eq!(tried(&c), [https]);
    // An address typed for an access token is the one they chose.
    let c = config(plain, Vec::new());
    assert_eq!(tried(&c), [plain]);
    // A linked source's list can come back empty: that doesn't make it typed.
    let mut c = config(plain, Vec::new());
    c.auth = Some(auth::PlexCredentials::manual(
        "account".into(),
        "client".into(),
    ));
    assert!(tried(&c).is_empty());
}

#[test]
fn a_server_that_stays_down_is_only_asked_once_in_a_while() {
    let down = Arc::new(AtomicBool::new(true));
    let is_down = down.clone();
    let server = TestServer::start(move |r| {
        if is_down.load(Ordering::Acquire) {
            return resp(503, Vec::new(), &[]);
        }
        let body = match r.path() {
            "/identity" => json!({"MediaContainer":{"machineIdentifier":"machine"}}),
            "/" | "/library/sections" => json!({"MediaContainer":{"Directory":[]}}),
            _ => return resp(404, Vec::new(), &[]),
        };
        resp(
            200,
            serde_json::to_vec(&body).unwrap(),
            &[("Content-Type", "application/json")],
        )
    });
    let source = PlexSource::new(&PlexConfig {
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
    .unwrap();
    let picture = "plex://source/source/machine/image/resource?path=%2Fa.jpg";
    assert!(source.picture_request(picture).is_none());
    let asked = server.requests().len();
    assert!(asked > 0);
    for _ in 0..20 {
        assert!(source.picture_request(picture).is_none());
        let error = source.open(picture).err().unwrap();
        assert!(
            !matches!(error, Error::NotFound(_)),
            "a server that is down hasn't deleted anything: {error}"
        );
    }
    assert_eq!(server.requests().len(), asked, "not asked again so soon");

    // Once that has passed, and the server is back, it connects.
    down.store(false, Ordering::Release);
    *lock(&source.unreachable) = None;
    assert!(source.probe().is_ok());
}

#[test]
fn deletion_respects_allow_media_deletion_and_invalidates_metadata_cache() {
    use std::sync::atomic::AtomicUsize;

    let deleted = Arc::new(AtomicUsize::new(0));
    for (allow_val, expect_allowed) in [
        (Some(json!(true)), true),
        (Some(json!(1)), true),
        (Some(json!(false)), false),
        (Some(json!(0)), false),
        (None, false),
    ] {
        let deleted_clone = deleted.clone();
        let allow_val = allow_val.clone();
        let server = TestServer::start(move |r| match (r.method.as_str(), r.path()) {
            ("GET", "/identity") => resp(
                200,
                serde_json::to_vec(&json!({"MediaContainer":{"machineIdentifier":"machine"}}))
                    .unwrap(),
                &[("Content-Type", "application/json")],
            ),
            ("GET", "/") => {
                let mut container = serde_json::Map::new();
                if let Some(v) = allow_val.clone() {
                    container.insert("allowMediaDeletion".into(), v);
                }
                resp(
                    200,
                    serde_json::to_vec(&json!({"MediaContainer": container})).unwrap(),
                    &[("Content-Type", "application/json")],
                )
            }
            ("GET", "/library/metadata/100") => resp(
                200,
                serde_json::to_vec(&json!({
                    "MediaContainer": {
                        "Metadata": [
                            {"ratingKey": "100", "type": "movie", "title": "Film"}
                        ]
                    }
                }))
                .unwrap(),
                &[("Content-Type", "application/json")],
            ),
            ("GET", "/library/metadata/200") => resp(
                200,
                serde_json::to_vec(&json!({
                    "MediaContainer": {
                        "Metadata": [
                            {"ratingKey": "200", "type": "show", "title": "Series"}
                        ]
                    }
                }))
                .unwrap(),
                &[("Content-Type", "application/json")],
            ),
            ("DELETE", "/library/metadata/100" | "/library/metadata/200") => {
                deleted_clone.fetch_add(1, Ordering::Relaxed);
                resp(200, Vec::new(), &[])
            }
            _ => resp(404, Vec::new(), &[]),
        });
        let source = PlexSource::new(&PlexConfig {
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
        .unwrap();
        let loc = "plex://source/source/machine/item/100";
        assert!(source.can_delete());
        assert_eq!(source.delete_allowed(loc), None);
        assert_eq!(source.ask_delete_allowed(loc).unwrap(), expect_allowed);
        assert_eq!(source.delete_allowed(loc), Some(expect_allowed));
        let item = source.item("100").unwrap();
        assert_eq!(item.can_delete, expect_allowed);

        let before = deleted.load(Ordering::Relaxed);
        let series = "plex://source/source/machine/item/200";
        assert!(!source.ask_delete_allowed(series).unwrap());
        assert!(matches!(
            source.delete(series),
            Err(Error::InvalidLocation { .. })
        ));
        assert_eq!(
            deleted.load(Ordering::Relaxed),
            before,
            "a series is never deleted"
        );

        if expect_allowed {
            let before = deleted.load(Ordering::Relaxed);
            source.delete(loc).unwrap();
            assert_eq!(deleted.load(Ordering::Relaxed), before + 1);
            assert!(
                !lock(&source.metadata).contains_key("100"),
                "metadata cache cleared after delete"
            );
        } else {
            assert!(matches!(source.delete(loc), Err(Error::Auth(_))));
        }
        assert!(matches!(
            source.delete("plex://source/source/machine/section/1"),
            Err(Error::InvalidLocation { .. })
        ));
    }
}

#[test]
fn display_location_is_the_servers_address_then_the_items() {
    let config = |id: &str| PlexConfig {
        id: id.into(),
        name: "Plex".into(),
        url: "https://plex.local:32400".into(),
        machine_identifier: "machine".into(),
        access_token: "secret".into(),
        connections: Vec::new(),
        auth: None,
        insecure_tls: false,
        parallel_downloads: false,
        max_height: None,
        local_only: false,
        allow_relay: false,
    };
    let s = PlexSource::new(&config("source")).unwrap();
    let other = PlexSource::new(&config("other")).unwrap();
    assert_eq!(
        s.display_location(None).as_deref(),
        Some("https://plex.local:32400")
    );
    let item = s.location("metadata", "42", Some("7"));
    assert_eq!(
        s.display_location(Some(&item)).as_deref(),
        Some("https://plex.local:32400 · 42")
    );
    assert_eq!(other.display_location(Some(&item)), None);
    assert!(!s.display_location(Some(&item)).unwrap().contains("secret"));
}
