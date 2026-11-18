use super::*;
use axum::{
    extract::Query,
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use std::collections::HashMap;

#[tokio::test]
async fn initialization_requires_both_absolute_folders_before_opening_storage() {
    let root = std::env::temp_dir().join(format!("deezer-folders-{}", uuid::Uuid::new_v4()));
    let data = root.join("chosen account");
    let cache = root.join("chosen cache");
    let adapter = Adapter::new(Endpoints::new(None, None, None));
    let greeting = json!({"protocol":1,"data_dir":data,"cache_dir":cache,"network":{"enabled":false}});
    for key in ["data_dir", "cache_dir"] {
        for value in [Value::Null, json!("relative"), json!(""), json!(7)] {
            let mut params = greeting.clone();
            params[key] = value;
            let error = adapter.handle("initialize", params).await.unwrap_err();
            assert_eq!(error.code, -32602);
            assert!(error.message.contains(key));
            assert!(!root.exists(), "A rejected greeting must not open storage");
        }
        let mut params = greeting.clone();
        params.as_object_mut().unwrap().remove(key);
        assert_eq!(adapter.handle("initialize", params).await.unwrap_err().code, -32602);
    }
    assert_eq!(adapter.handle("settings.open", json!({})).await.unwrap_err().code, -32600);
    assert_eq!(adapter.handle("initialize", greeting).await.unwrap()["protocol"], 1);
    assert_eq!(adapter.data(), data);
    assert!(data.is_dir() && cache.is_dir());
    assert_eq!(adapter.handle("initialize", json!({"protocol":1,"data_dir":root.join("other"),"cache_dir":cache})).await.unwrap_err().code, -32600);
    assert!(!root.join("other").exists());
    std::fs::remove_dir_all(root).unwrap();
}

async fn gateway(
    Query(query): Query<HashMap<String, String>>,
    Json(args): Json<Value>,
) -> Json<Value> {
    let result = match query.get("method").map(String::as_str) {
        Some("deezer.getUserData") => {
            json!({"checkForm":"synthetic-form","USER":{"USER_ID":9000,"BLOG_NAME":"Test account","OPTIONS":{"web_lossless":true,"web_hq":true,"license_token":"synthetic-only"}}})
        }
        Some("deezer.pageProfile") => {
            if args["tab"] == "playlists" {
                json!({"TAB":{"playlists":{"data":[{"PLAYLIST_ID":"91","TITLE":"Hashed","PICTURE_TYPE":"PLAYLIST","PLAYLIST_PICTURE":"aabbcc"},{"PLAYLIST_ID":"92","TITLE":"Sparse"}]}}})
            } else {
                json!({"TAB":{"albums":{"data":(1..=601).map(|id|json!({"ALB_ID":id,"ALB_TITLE":format!("Album {id:04}"),"ART_NAME":"Test artist","ART_ID":2,"NUMBER_TRACK":4})).collect::<Vec<_>>()}}})
            }
        }
        Some("playlist.getData") => {
            return Json(
                json!({"error":{"GATEWAY_ERROR":"Undefined or invalid output"},"results":{}}),
            );
        }
        Some("deezer.pagePlaylist") => {
            assert_eq!(args["nb"], 0, "Header reads must not fetch embedded tracks");
            if args["PLAYLIST_ID"] == "95" {
                return Json(json!({"error":[],"results":{"SONGS":{"data":[]}}}));
            }
            if args["PLAYLIST_ID"] == "96" {
                return Json(json!({"error":{"DATA_ERROR":"Playlist unavailable"},"results":{}}));
            }
            let total = match args["PLAYLIST_ID"].as_str().unwrap_or_default() {
                "92" => "1001",
                "93" => "10001",
                "97" => "0",
                _ => "3",
            };
            json!({"DATA":{"PLAYLIST_ID":args["PLAYLIST_ID"],"TITLE":"Fixture playlist","NB_SONG":total},
                "SONGS":{"data":[],"total":total}})
        }
        Some("playlist.getSongs") => {
            let total = match args["PLAYLIST_ID"].as_str().unwrap_or_default() {
                "91" => 3,
                "92" => 1001,
                "93" => 10001,
                "97" => 0,
                _ => 3,
            };
            let start = args["start"].as_u64().unwrap_or(0);
            let end = (start + args["nb"].as_u64().unwrap_or(200)).min(total);
            let songs = if args["PLAYLIST_ID"] == "94" {
                Vec::new()
            } else {
                (start..end).map(|at| json!({"SNG_ID":at+1,"SNG_TITLE":format!("Track {}",at+1),
                    "ART_ID":"2","ART_NAME":"Fixture artist","ALB_ID":"3","ALB_TITLE":"Fixture album","DURATION":"180"})).collect()
            };
            json!({"data":songs,"total":total.to_string()})
        }
        _ => json!({}),
    };
    Json(json!({"error":[],"results":result}))
}

#[tokio::test]
async fn playlist_tracks_follow_host_bounds_and_do_not_stop_at_one_thousand() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/ajax/gw-light.php", post(gateway)),
        )
        .await
        .unwrap();
    });
    let adapter = Adapter::new(Endpoints::new(
        Some(&endpoint),
        Some(&endpoint),
        Some(&endpoint),
    ));
    let data = std::env::temp_dir().join(format!("deezer-playlist-test-{}", uuid::Uuid::new_v4()));
    adapter
        .handle(
            "initialize",
            json!({"protocol":1,"data_dir":data,"cache_dir":data.join("cache"),"locale":"en","network":{"enabled":true}}),
        )
        .await
        .unwrap();
    let signed_in = adapter.client().with_arl(&"a".repeat(64));
    *adapter.client.lock().unwrap() = signed_in;
    for (id, length) in [("91", 3), ("92", 1001)] {
        let result = adapter
            .handle(
                "playlist.get",
                json!({"ref":format!("playlist:{id}"),"offset":0,"limit":10000}),
            )
            .await
            .unwrap();
        assert_eq!(result["playlist"]["ref"], format!("playlist:{id}"));
        assert_eq!(result["playlist"]["title"], "Fixture playlist");
        assert_eq!(result["playlist"]["tracks"], length);
        assert_eq!(result["tracks"].as_array().unwrap().len(), length);
        assert_eq!(result["tracks"][0]["ref"], "track:1");
        assert_eq!(
            result["tracks"][length - 1]["ref"],
            format!("track:{length}")
        );
        assert_eq!(result["tracks"][length - 1]["album_ref"], "album:3");
    }
    let page = adapter
        .handle("page.get", json!({"ref":"playlist:92"}))
        .await
        .unwrap();
    assert_eq!(page["shelves"][0]["items"].as_array().unwrap().len(), 1001);
    let repeated = adapter
        .handle("page.get", json!({"ref":"playlist:92","refresh":true}))
        .await
        .unwrap();
    assert_eq!(
        page, repeated,
        "Equal reads must preserve native rows and identity"
    );
    let empty = adapter
        .handle("playlist.get", json!({"ref":"playlist:97"}))
        .await
        .unwrap();
    assert_eq!(empty["playlist"]["tracks"], 0);
    assert_eq!(empty["tracks"], json!([]));
    for id in ["95", "96"] {
        let error = adapter
            .handle("playlist.get", json!({"ref":format!("playlist:{id}")}))
            .await
            .unwrap_err();
        assert_eq!(
            error.code, -32002,
            "Unreadable playlists must remain errors"
        );
    }
    for (id, text) in [("93", "requested track limit"), ("94", "non-advancing")] {
        let error = adapter
            .handle(
                "playlist.get",
                json!({"ref":format!("playlist:{id}"),"limit":10000}),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, -32003);
        assert!(error.message.contains(text));
    }
    std::fs::remove_dir_all(data).unwrap();
    server.abort();
}

#[tokio::test]
async fn home_view_all_includes_preview_rows_and_pages_the_complete_listing() {
    async fn catalogue(
        axum::extract::Path(path): axum::extract::Path<String>,
        Query(query): Query<HashMap<String, String>>,
    ) -> Json<Value> {
        let tracks = path.ends_with("/tracks");
        let total = if path.starts_with("editorial/") {
            3
        } else {
            405
        };
        let from = query
            .get("index")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let limit = query
            .get("limit")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(200);
        let rows: Vec<_> = (from..(from+limit).min(total)).map(|at| {
            if tracks { json!({"id":at+1,"title":format!("Track {at}"),"duration":180,"artist":{"id":2,"name":"Fixture artist"},"album":{"id":3,"title":"Fixture album"}}) }
            else { json!({"id":at+1,"title":format!("Album {at}"),"artist":{"id":2,"name":"Fixture artist"}}) }
        }).collect();
        Json(json!({"data":rows,"total":total}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/{*path}", get(catalogue)))
            .await
            .unwrap();
    });
    let adapter = Adapter::new(Endpoints::new(None, Some(&endpoint), None));
    let data = std::env::temp_dir().join(format!("deezer-home-test-{}", uuid::Uuid::new_v4()));
    adapter
        .handle(
            "initialize",
            json!({"protocol":1,"data_dir":data,"cache_dir":data.join("cache"),"locale":"en","network":{"enabled":true}}),
        )
        .await
        .unwrap();
    let home = adapter
        .handle("page.get", json!({"ref":"discover:home"}))
        .await
        .unwrap();
    for (index, original, total) in [
        (0, "releases", 3),
        (1, "charts/albums", 405),
        (2, "charts/tracks", 405),
    ] {
        let shelf = &home["shelves"][index];
        assert_eq!(shelf["more"], format!("more:{original}:0"));
        let mut reference = shelf["more"].as_str().unwrap().to_owned();
        let mut rows = Vec::new();
        loop {
            let page = adapter
                .handle("page.get", json!({"ref":reference}))
                .await
                .unwrap();
            let values = page["items"].as_array().unwrap();
            if rows.is_empty() {
                assert_eq!(values, shelf["items"].as_array().unwrap());
            }
            rows.extend(values.iter().cloned());
            match page["next"].as_str() {
                Some(next) => reference = next.to_owned(),
                None => break,
            }
        }
        assert_eq!(rows.len(), total);
        assert_eq!(
            rows.iter()
                .filter_map(|row| row["ref"].as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            total
        );
    }
    std::fs::remove_dir_all(data).unwrap();
    server.abort();
}

#[tokio::test]
async fn guided_account_connects_validates_library_pages_and_signs_out() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/ajax/gw-light.php", post(gateway)),
        )
        .await
        .unwrap();
    });
    let adapter = Adapter::new(Endpoints::new(
        Some(&endpoint),
        Some(&endpoint),
        Some(&endpoint),
    ));
    let data = std::env::temp_dir().join(format!("deezer-source-test-{}", uuid::Uuid::new_v4()));
    let greeting=adapter.handle("initialize",json!({"protocol":1,"data_dir":data,"cache_dir":data,"locale":"es","network":{"enabled":true}})).await.unwrap();
    assert_eq!(
        greeting["capabilities"]["library"],
        json!(["albums", "tracks", "artists", "playlists"])
    );
    let browser = adapter.handle("settings.open", json!({})).await.unwrap();
    assert_eq!(browser["wizard"]["id"], "browser");
    let session = adapter
        .handle(
            "settings.submit",
            json!({"revision":browser["revision"],"action":"continue","values":{}}),
        )
        .await
        .unwrap();
    assert_eq!(session["wizard"]["id"], "session");
    let invalid=adapter.handle("settings.submit",json!({"revision":session["revision"],"action":"sign_in","values":{"session":"https://www.deezer.com/login"}})).await.unwrap();
    assert_eq!(invalid["wizard"]["id"], "session");
    assert!(!invalid["notices"].as_array().unwrap().is_empty());
    assert!(!data.join("account.json").exists());
    let token = "a".repeat(64);
    let connected=adapter.handle("settings.submit",json!({"revision":invalid["revision"],"action":"sign_in","values":{"session":token,"quality":"mp3_320"}})).await.unwrap();
    assert_eq!(connected["wizard"]["id"], "connected");
    assert!(!connected.to_string().contains(&token));
    assert_eq!(connected["values"]["quality"], "mp3_320");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(data.join("account.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let library = adapter
        .handle(
            "library.list",
            json!({"kind":"albums","offset":0,"limit":500,"refresh":true}),
        )
        .await
        .unwrap();
    assert_eq!(library["items"].as_array().unwrap().len(), 500);
    assert_eq!(library["total"], 601);
    assert_eq!(library["items"][499]["ref"], "album:500");
    let final_page = adapter
        .handle(
            "library.list",
            json!({"kind":"albums","offset":500,"limit":500}),
        )
        .await
        .unwrap();
    assert_eq!(final_page["items"].as_array().unwrap().len(), 101);
    let playlists = adapter
        .handle(
            "library.list",
            json!({"kind":"playlists","offset":0,"limit":500}),
        )
        .await
        .unwrap();
    assert_eq!(
        playlists["items"][0]["image"],
        "https://cdn-images.dzcdn.net/images/playlist/aabbcc/500x500-000000-80-0-0.jpg"
    );
    assert_eq!(
        playlists["items"][1]["image"],
        "https://api.deezer.com/playlist/92/image?size=big"
    );
    assert!(adapter
        .handle(
            "settings.submit",
            json!({"revision":browser["revision"],"action":"sign_out","values":{}})
        )
        .await
        .is_err());
    let signed_out = adapter
        .handle(
            "settings.submit",
            json!({"revision":connected["revision"],"action":"sign_out","values":{}}),
        )
        .await
        .unwrap();
    assert_eq!(signed_out["wizard"]["id"], "browser");
    assert!(!data.join("account.json").exists());
    assert_eq!(
        adapter
            .handle("track.resolve", json!({"ref":"track:7"}))
            .await
            .unwrap_err()
            .code,
        -32001
    );
    std::fs::remove_dir_all(data).unwrap();
    server.abort();
}

#[tokio::test]
async fn catalogue_language_follows_host_locale_instead_of_country() {
    async fn genre(
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Json<Value> {
        Json(
            json!({"lang":query.get("lang"),"header":headers.get("accept-language").and_then(|v|v.to_str().ok())}),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/genre", get(genre)))
            .await
            .unwrap()
    });
    let adapter = Adapter::new(Endpoints::new(None, Some(&endpoint), None));
    let data = std::env::temp_dir().join(format!("deezer-language-test-{}", uuid::Uuid::new_v4()));
    adapter
        .handle(
            "initialize",
            json!({"protocol":1,"data_dir":data,"cache_dir":data.join("cache"),"locale":"en_US.UTF-8","network":{"enabled":true}}),
        )
        .await
        .unwrap();
    let result = adapter
        .client()
        .api("genre", vec![("lang", "es".into())])
        .await
        .unwrap();
    assert_eq!(result["lang"], "en");
    assert_eq!(result["header"], "en");
    let spanish = adapter
        .client()
        .with_locale("es-MX")
        .api("genre", vec![])
        .await
        .unwrap();
    assert_eq!(spanish["lang"], "es");
    assert_eq!(spanish["header"], "es");
    let default = adapter
        .client()
        .with_locale("C")
        .api("genre", vec![])
        .await
        .unwrap();
    assert_eq!(default["lang"], "en");
    std::fs::remove_dir_all(data).unwrap();
    server.abort();
}

#[tokio::test]
async fn offline_mode_does_not_validate_a_saved_account_or_resolve_audio() {
    let data = std::env::temp_dir().join(format!("deezer-source-offline-{}", uuid::Uuid::new_v4()));
    let adapter = Adapter::new(Endpoints::new(Some("http://127.0.0.1:1"), None, None));
    adapter
        .handle(
            "initialize",
            json!({"protocol":1,"data_dir":data,"cache_dir":data.join("cache"),"network":{"enabled":false}}),
        )
        .await
        .unwrap();
    let form = adapter.handle("settings.open", json!({})).await.unwrap();
    assert_eq!(form["wizard"]["waiting"], false);
    assert_eq!(
        adapter
            .handle("track.resolve", json!({"ref":"track:7"}))
            .await
            .unwrap_err()
            .code,
        -32005
    );
    assert!(!data.join("account.json").exists());
    std::fs::remove_dir_all(data).unwrap();
}
