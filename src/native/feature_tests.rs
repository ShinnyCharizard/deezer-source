use super::*;
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use std::collections::HashMap;

#[derive(Default)]
struct Service {
    calls: Vec<(String, Value)>,
    tracks: Vec<String>,
    title: String,
    description: String,
    status: u64,
    bad_create: bool,
    bad_feed: bool,
    empty_feed: bool,
    deleted: bool,
}

fn album(id: usize) -> Value {
    json!({"__TYPE__":"album","ALB_ID":id,"ALB_TITLE":format!("Album {id}"),"ART_ID":2,"ART_NAME":"Fixture artist"})
}

fn playlist(id: &str, service: &Service) -> Value {
    json!({"PLAYLIST_ID":id,"TITLE":service.title,"DESCRIPTION":service.description,
        "STATUS":service.status,"PARENT_USER_ID":if id=="502" {9001} else {9000},"NB_SONG":service.tracks.len()})
}

async fn gateway(
    State(service): State<Arc<Mutex<Service>>>,
    Query(query): Query<HashMap<String, String>>,
    Json(args): Json<Value>,
) -> Json<Value> {
    let method = query.get("method").map(String::as_str).unwrap_or_default();
    let mut service = service.lock().unwrap();
    service.calls.push((method.to_owned(), args.clone()));
    let results = match method {
        "deezer.getUserData" => {
            json!({"checkForm":"fixture-form","USER":{"USER_ID":9000,"BLOG_NAME":"Fixture account"}})
        }
        "deezer.pageProfile" => {
            json!({"TAB":{"playlists":{"data":if service.deleted {vec![]} else {vec![playlist("501", &service)]}}}})
        }
        "deezer.pagePlaylist" => {
            assert_eq!(args["nb"], 0);
            json!({"DATA":playlist(args["PLAYLIST_ID"].as_str().unwrap(), &service)})
        }
        "playlist.getSongs" => {
            let start = args["start"].as_u64().unwrap_or(0) as usize;
            let count = args["nb"].as_u64().unwrap_or(200) as usize;
            json!({"total":service.tracks.len(),"data":service.tracks.iter().skip(start).take(count)
                .map(|id|json!({"SNG_ID":id,"SNG_TITLE":format!("Song {id}"),"ART_ID":2,"ART_NAME":"Fixture artist"})).collect::<Vec<_>>()})
        }
        "playlist.create" => {
            assert_eq!(args["songs"], json!([]));
            service.title = args["title"].as_str().unwrap().to_owned();
            service.description = args["description"].as_str().unwrap().to_owned();
            service.status = args["status"].as_u64().unwrap();
            if service.bad_create {
                Value::Null
            } else {
                json!(501)
            }
        }
        "playlist.addSongs" => {
            assert_eq!(args["offset"], -1);
            if args["PLAYLIST_ID"] == "503" {
                return Json(
                    json!({"error":{"VALID_TOKEN_REQUIRED":"Invalid CSRF token"},"results":{}}),
                );
            }
            if args["PLAYLIST_ID"] == "504" {
                return Json(json!({"error":{"DATA_ERROR":"Write refused"},"results":{}}));
            }
            if args["PLAYLIST_ID"] == "505" {
                return Json(json!({"error":[],"results":null}));
            }
            for song in args["songs"].as_array().unwrap() {
                assert_eq!(song[1], 0);
                service.tracks.push(song[0].as_str().unwrap().to_owned());
            }
            json!(true)
        }
        "playlist.update" => {
            assert!(
                args.get("songs").is_none(),
                "Metadata edits must not replace playlist tracks"
            );
            service.title = args["title"].as_str().unwrap().to_owned();
            service.description = args["description"].as_str().unwrap().to_owned();
            service.status = args["status"].as_u64().unwrap();
            json!(true)
        }
        "playlist.delete" => {
            service.deleted = true;
            json!(true)
        }
        "page.get" => {
            assert_eq!(args["LANG"], "es");
            assert_eq!(args["VERSION"], "2.5");
            if service.bad_feed {
                return Json(json!({"error":[],"results":{}}));
            }
            if service.empty_feed {
                json!({"sections":[]})
            } else if args["PAGE"] == "home" {
                json!({"render_id":service.calls.len(),"sections":[
                    {"section_id":format!("random-{}",service.calls.len()),"module_id":"artists","title":"Tus artistas favoritos",
                        "items":[{"data":{"__TYPE__":"artist","ART_ID":"2","ART_NAME":"Fixture artist"}}]},
                    {"module_id":"for-you","title":"Hecho para ti","target":"/channels/module/for-you","hasMoreItems":true,
                        "items":[{"data":{"__TYPE__":"playlist","PLAYLIST_ID":"501","TITLE":"My mix","PARENT_USER_ID":9000,"STATUS":1}},
                            {"data":{"__TYPE__":"playlist","PLAYLIST_ID":"502","TITLE":"Curated","PARENT_USER_ID":9001,"STATUS":0}}]},
                    {"module_id":"releases","title":"Novedades para ti","target":"/channels/module/releases","hasMoreItems":true,
                        "items":(1..=24).map(|id|json!({"data":album(id)})).collect::<Vec<_>>()},
                    {"module_id":"mixed","title":"Descubre más","items":[{"data":album(7)},
                        {"data":{"__TYPE__":"artist","ART_ID":"2","ART_NAME":"Fixture artist"}},
                        {"data":{"__TYPE__":"song","SNG_ID":"8","SNG_TITLE":"Song eight","ART_ID":"2","ART_NAME":"Fixture artist"}}]},
                    {"module_id":"unsupported","title":"Podcasts","items":[{"data":{"__TYPE__":"podcast","id":"1"}}]}
                ]})
            } else if args["PAGE"] == "channels/module/releases" {
                json!({"title":"Novedades para ti","sections":[{"module_id":"releases","items":(1..=205).map(|id|json!({"data":album(id)})).collect::<Vec<_>>()}]})
            } else {
                json!({"title":"Hecho para ti","sections":[{"module_id":"for-you","items":[
                    {"data":{"__TYPE__":"playlist","PLAYLIST_ID":"501","TITLE":"My mix","PARENT_USER_ID":9000,"STATUS":1}},
                    {"data":{"__TYPE__":"playlist","PLAYLIST_ID":"502","TITLE":"Curated","PARENT_USER_ID":9001,"STATUS":0}}
                ]}]})
            }
        }
        _ => panic!("Unexpected gateway method: {method}"),
    };
    Json(json!({"error":[],"results":results}))
}

async fn search(Path(path): Path<String>) -> Json<Value> {
    Json(if path == "search/track" {
        json!({"data":[{"id":8,"title":"Found song","artist":{"id":2,"name":"Fixture artist"}}],"total":1})
    } else {
        json!({"data":[],"total":0})
    })
}

async fn setup() -> (
    Adapter,
    Arc<Mutex<Service>>,
    tokio::task::JoinHandle<()>,
    PathBuf,
) {
    let service = Arc::new(Mutex::new(Service {
        title: "Existing".into(),
        status: 1,
        ..Default::default()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/ajax/gw-light.php", post(gateway))
        .route("/{*path}", get(search))
        .with_state(service.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let adapter = Adapter::new(Endpoints::new(
        Some(&endpoint),
        Some(&endpoint),
        Some(&endpoint),
    ));
    let data = std::env::temp_dir().join(format!("deezer-features-{}", uuid::Uuid::new_v4()));
    adapter
        .handle(
            "initialize",
            json!({"protocol":1,"data_dir":data,"cache_dir":data.join("cache"),"locale":"es-MX","network":{"enabled":true}}),
        )
        .await
        .unwrap();
    let signed_in = adapter.client().with_arl(&"a".repeat(64));
    *adapter.client.lock().unwrap() = signed_in;
    (adapter, service, server, data)
}

fn writes(service: &Arc<Mutex<Service>>) -> Vec<(String, Value)> {
    service
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(method, _)| {
            matches!(
                method.as_str(),
                "playlist.create" | "playlist.addSongs" | "playlist.update" | "playlist.delete"
            )
        })
        .cloned()
        .collect()
}

#[tokio::test]
async fn native_playlist_creation_and_addition_reach_the_account_and_refresh_the_library() {
    let (adapter, service, server, data) = setup().await;
    let before = adapter
        .handle("library.list", json!({"kind":"playlists"}))
        .await
        .unwrap();
    assert_eq!(before["items"][0]["title"], "Existing");
    let created = adapter.handle("page.action",json!({"action":"playlist.create","title":" New playlist ","description":"A description","public":false})).await.unwrap();
    assert_eq!(created["item"]["ref"], "playlist:501");
    assert_eq!(created["item"]["editable"], true);
    assert_eq!(created["item"]["public"], false);
    assert_eq!(created["changed_refs"], json!(["playlist:501"]));
    let library = adapter
        .handle("library.list", json!({"kind":"playlists"}))
        .await
        .unwrap();
    assert_eq!(library["items"][0]["title"], "New playlist");
    assert_eq!(library["items"][0]["editable"], true);
    assert_eq!(library["items"][0]["description"], "A description");
    let added = adapter.handle("page.action",json!({"action":"playlist.add","ref":created["item"]["ref"],"tracks":["track:8","track:9"]})).await.unwrap();
    assert_eq!(added["changed_refs"], json!(["playlist:501"]));
    let read = adapter
        .handle("playlist.get", json!({"ref":"playlist:501"}))
        .await
        .unwrap();
    assert_eq!(
        read["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["ref"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["track:8", "track:9"]
    );
    assert_eq!(read["playlist"]["editable"], true);
    let refreshed = adapter
        .handle("library.list", json!({"kind":"playlists"}))
        .await
        .unwrap();
    assert_eq!(refreshed["items"][0]["tracks"], 2);
    adapter.handle("page.action",json!({"action":"playlist.update","ref":"playlist:501","title":"Renamed","description":"Changed","public":true})).await.unwrap();
    let updated = adapter
        .handle("playlist.get", json!({"ref":"playlist:501"}))
        .await
        .unwrap();
    assert_eq!(updated["playlist"]["title"], "Renamed");
    assert_eq!(updated["playlist"]["public"], true);
    assert_eq!(updated["tracks"], read["tracks"]);
    let found = adapter
        .handle(
            "page.action",
            json!({"action":"playlist.search","ref":"playlist:501","text":"Found"}),
        )
        .await
        .unwrap();
    assert_eq!(found["read_only"], true);
    assert_eq!(found["items"][0]["ref"], "track:8");
    assert_eq!(writes(&service).len(), 3);
    adapter
        .handle(
            "page.action",
            json!({"action":"playlist.delete","ref":"playlist:501"}),
        )
        .await
        .unwrap();
    let deleted = adapter
        .handle("library.list", json!({"kind":"playlists"}))
        .await
        .unwrap();
    assert_eq!(deleted["items"], json!([]));
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn playlist_writes_reject_foreign_ownership_and_invalid_batches_before_mutation() {
    let (adapter, service, server, data) = setup().await;
    for action in ["playlist.add", "playlist.update", "playlist.delete"] {
        let error = adapter.handle("page.action",json!({"action":action,"ref":"playlist:502","tracks":["track:8"],"title":"Foreign"})).await.unwrap_err();
        assert_eq!(error.code, -32003);
    }
    for tracks in [
        json!([]),
        json!(["track:8", "album:9"]),
        json!(["track:0"]),
        json!(vec!["track:8"; 101]),
    ] {
        assert!(adapter
            .handle(
                "page.action",
                json!({"action":"playlist.add","ref":"playlist:501","tracks":tracks})
            )
            .await
            .is_err());
    }
    for fields in [
        json!({"title":" "}),
        json!({"title":"Valid","public":"true"}),
        json!({"title":"Valid","description":3}),
    ] {
        let mut input = fields;
        input["action"] = json!("playlist.create");
        assert_eq!(
            adapter.handle("page.action", input).await.unwrap_err().code,
            -32602
        );
    }
    assert!(writes(&service).is_empty());
    adapter.online.store(false, Ordering::Release);
    assert_eq!(
        adapter
            .handle(
                "page.action",
                json!({"action":"playlist.create","title":"Offline"})
            )
            .await
            .unwrap_err()
            .code,
        -32005
    );
    assert!(writes(&service).is_empty());
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn rejected_or_unconfirmed_playlist_writes_are_never_retried_or_reported_as_success() {
    let (adapter, service, server, data) = setup().await;
    for (id, code) in [("503", -32001), ("504", -32002), ("505", -32006)] {
        let error=adapter.handle("page.action",json!({"action":"playlist.add","ref":format!("playlist:{id}"),"tracks":["track:8"]})).await.unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(
            writes(&service)
                .iter()
                .filter(|(_, args)| args["PLAYLIST_ID"] == id)
                .count(),
            1
        );
    }
    service.lock().unwrap().bad_create = true;
    assert_eq!(
        adapter
            .handle(
                "page.action",
                json!({"action":"playlist.create","title":"Created remotely"})
            )
            .await
            .unwrap_err()
            .code,
        -32006
    );
    assert_eq!(service.lock().unwrap().title, "Created remotely");
    assert_eq!(writes(&service).len(), 4);
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn personalised_home_preserves_module_identity_and_complete_view_all() {
    let (adapter, service, server, data) = setup().await;
    let page = adapter
        .handle("page.get", json!({"ref":"discover:home"}))
        .await
        .unwrap();
    let repeated = adapter
        .handle("page.get", json!({"ref":"discover:home","refresh":true}))
        .await
        .unwrap();
    assert_eq!(
        page, repeated,
        "Tracking IDs must not change native shelf identity"
    );
    assert_eq!(page["personal"], true);
    assert_eq!(page["shelves"].as_array().unwrap().len(), 4);
    assert_eq!(page["shelves"][0]["items"][0]["ref"], "artist:2");
    assert_eq!(page["shelves"][1]["items"][0]["editable"], true);
    assert_eq!(page["shelves"][1]["items"][1]["editable"], false);
    assert_eq!(page["shelves"][3]["kind"], "mixed");
    assert_eq!(page["shelves"][3]["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["shelves"][3]["extra_tracks"][0]["ref"], "track:8");
    let shelf = &page["shelves"][2];
    assert_eq!(shelf["id"], "feed-releases");
    for album in shelf["items"].as_array().unwrap() {
        assert_eq!(album["artist_ref"], "artist:2", "Album credits must link to their source artist");
    }
    let first = adapter
        .handle("page.get", json!({"ref":shelf["more"]}))
        .await
        .unwrap();
    assert_eq!(
        &first["items"].as_array().unwrap()[..24],
        shelf["items"].as_array().unwrap()
    );
    let next = adapter
        .handle("page.get", json!({"ref":first["next"]}))
        .await
        .unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 200);
    assert_eq!(next["items"].as_array().unwrap().len(), 5);
    assert_eq!(next["items"][0]["ref"], "album:201");
    assert!(next.get("next").is_none());
    for target in ["feed:../bad:0", "feed:releases:1001", "feed:releases:-1"] {
        assert_eq!(
            adapter
                .handle("page.get", json!({"ref":target}))
                .await
                .unwrap_err()
                .code,
            -32602
        );
    }
    service.lock().unwrap().bad_feed = true;
    assert_eq!(
        adapter
            .handle("page.get", json!({"ref":"discover:home","refresh":true}))
            .await
            .unwrap_err()
            .code,
        -32003
    );
    service.lock().unwrap().bad_feed = false;
    service.lock().unwrap().empty_feed = true;
    assert_eq!(
        adapter
            .handle("page.get", json!({"ref":"discover:home"}))
            .await
            .unwrap_err()
            .code,
        -32003
    );
    service.lock().unwrap().empty_feed = false;
    assert_eq!(
        adapter
            .handle("page.get", json!({"ref":"discover:home"}))
            .await
            .unwrap(),
        page
    );
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}
