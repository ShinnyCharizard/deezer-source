use super::*;
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use std::collections::HashMap;

struct Catalogue {
    calls: Vec<String>,
    quota: bool,
    malformed: bool,
    gateway_tracks: usize,
}

fn gateway_track(index: usize) -> Value {
    json!({"SNG_ID":index+10,"SNG_TITLE":format!("Song {index}"),"ART_ID":2,
        "ART_NAME":"Artist","ALB_ID":3,"ALB_TITLE":"Album","ISRC":format!("FIXTURE{index:05}")})
}

async fn gateway(
    State(state): State<Arc<Mutex<Catalogue>>>,
    Query(query): Query<HashMap<String, String>>,
    Json(args): Json<Value>,
) -> Json<Value> {
    let method = query["method"].as_str();
    let mut state = state.lock().unwrap();
    state.calls.push(method.to_owned());
    let results = match method {
        "deezer.getUserData" => json!({"checkForm":"fixture-form","USER":{"USER_ID":9000}}),
        "deezer.pageAlbum" => {
            assert_eq!(args["alb_id"], "3");
            assert_eq!(args["lang"], "en");
            assert_eq!(args["tab"], 0);
            assert_eq!(args["header"], true);
            if state.quota {
                return Json(json!({"error":{"QUOTA":"Rate limit exceeded"},"results":{}}));
            }
            if state.malformed {
                json!({"DATA":{"ALB_ID":99,"ALB_TITLE":"Wrong album"},"SONGS":{"data":[],"total":0}})
            } else {
                json!({"DATA":{"ALB_ID":3,"ALB_TITLE":"Album","ART_ID":2,"ART_NAME":"Artist","NUMBER_TRACK":4,
                    "DURATION":"720","STYLE_NAME":"Rock","EXPLICIT_ALBUM_CONTENT":{"EXPLICIT_LYRICS_STATUS":1}},
                    "SONGS":{"data":(0..state.gateway_tracks).map(gateway_track).collect::<Vec<_>>(),"total":4}})
            }
        }
        "song.getListByAlbum" => {
            let start = args["start"].as_u64().unwrap() as usize;
            let count = args["nb"].as_u64().unwrap() as usize;
            json!({"data":(start..4).take(count).map(gateway_track).collect::<Vec<_>>(),"total":4})
        }
        _ => panic!("Unexpected gateway method {method}"),
    };
    Json(json!({"error":[],"results":results}))
}

async fn public(
    State(state): State<Arc<Mutex<Catalogue>>>,
    Path(path): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    let index = query
        .get("index")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    state.lock().unwrap().calls.push(format!("{path}:{index}"));
    Json(match path.as_str() {
        "album/3" => {
            json!({"id":3,"title":"Album","artist":{"id":2,"name":"Artist"},"nb_tracks":4})
        }
        "album/3/tracks" => {
            let limit = query
                .get("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(200);
            json!({"total":4,"data":(index..4).take(limit).map(|i|json!({"id":i+10,"title":format!("Song {i}"),"artist":{"id":2,"name":"Artist"}})).collect::<Vec<_>>()})
        }
        _ => panic!("Unexpected public path {path}"),
    })
}

async fn setup(
    signed_in: bool,
) -> (
    Adapter,
    Arc<Mutex<Catalogue>>,
    tokio::task::JoinHandle<()>,
    PathBuf,
) {
    let state = Arc::new(Mutex::new(Catalogue {
        calls: vec![],
        quota: false,
        malformed: false,
        gateway_tracks: 4,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/ajax/gw-light.php", post(gateway))
        .route("/{*path}", get(public))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let adapter = Adapter::new(Endpoints::new(
        Some(&endpoint),
        Some(&endpoint),
        Some(&endpoint),
    ));
    let data = std::env::temp_dir().join(format!("deezer-catalogue-{}", uuid::Uuid::new_v4()));
    adapter.handle("initialize", json!({"protocol":1,"data_dir":data,"cache_dir":data.join("cache"),"locale":"en","network":{"enabled":true}})).await.unwrap();
    if signed_in {
        let signed_in = adapter.client().with_arl(&"a".repeat(64));
        *adapter.client.lock().unwrap() = signed_in;
    }
    (adapter, state, server, data)
}

#[tokio::test]
async fn signed_in_album_reads_header_and_tracks_in_one_gateway_call() {
    let (adapter, state, server, data) = setup(true).await;
    let result = adapter
        .handle("album.get", json!({"ref":"album:3"}))
        .await
        .unwrap();
    assert_eq!(result["tracks"].as_array().unwrap().len(), 4);
    assert_eq!(
        state.lock().unwrap().calls,
        ["deezer.getUserData", "deezer.pageAlbum"]
    );
    assert_eq!(result["tracks"][0]["isrc"], "FIXTURE00000");
    assert_eq!(result["album"]["duration_ms"], 720000);
    assert_eq!(result["album"]["genre"], "Rock");
    assert_eq!(result["album"]["explicit"], true);
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn signed_out_album_fetches_its_public_header_only_once() {
    let (adapter, state, server, data) = setup(false).await;
    let result = adapter
        .handle("album.get", json!({"ref":"album:3"}))
        .await
        .unwrap();
    assert_eq!(result["tracks"].as_array().unwrap().len(), 4);
    assert_eq!(
        state.lock().unwrap().calls,
        ["album/3:0", "album/3/tracks:0"]
    );
    assert_eq!(result["tracks"][0]["album_ref"], "album:3");
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn a_partial_gateway_album_continues_after_its_embedded_tracks() {
    let (adapter, state, server, data) = setup(true).await;
    state.lock().unwrap().gateway_tracks = 2;
    let result = adapter
        .handle("album.get", json!({"ref":"album:3"}))
        .await
        .unwrap();
    assert_eq!(
        result["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["ref"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["track:10", "track:11", "track:12", "track:13"]
    );
    assert_eq!(
        state.lock().unwrap().calls,
        [
            "deezer.getUserData",
            "deezer.pageAlbum",
            "song.getListByAlbum"
        ]
    );
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn gateway_quota_does_not_fan_out_and_can_recover_on_the_next_read() {
    let (adapter, state, server, data) = setup(true).await;
    state.lock().unwrap().quota = true;
    let error = adapter
        .handle("album.get", json!({"ref":"album:3"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, -32004);
    assert_eq!(
        adapter.safe_error(error).data,
        Some(json!({"retry_after":5}))
    );
    assert_eq!(
        state.lock().unwrap().calls,
        ["deezer.getUserData", "deezer.pageAlbum"]
    );
    assert_eq!(
        adapter
            .handle("album.get", json!({"ref":"album:3"}))
            .await
            .unwrap_err()
            .code,
        -32004
    );
    assert_eq!(
        state.lock().unwrap().calls.len(),
        2,
        "A cooldown must not send another request"
    );
    state.lock().unwrap().quota = false;
    assert_eq!(
        adapter
            .handle("album.get", json!({"ref":"album:3","refresh":true}))
            .await
            .unwrap()["tracks"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn album_bounds_are_validated_before_any_catalogue_request() {
    let (adapter, state, server, data) = setup(true).await;
    for params in [
        json!({"ref":"album:3","offset":-1}),
        json!({"ref":"album:3","limit":10001}),
    ] {
        assert_eq!(
            adapter.handle("album.get", params).await.unwrap_err().code,
            -32602
        );
    }
    assert!(state.lock().unwrap().calls.is_empty());
    state.lock().unwrap().malformed = true;
    assert!(adapter
        .handle("album.get", json!({"ref":"album:3"}))
        .await
        .is_err());
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}
