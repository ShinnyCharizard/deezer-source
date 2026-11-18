use super::*;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use std::collections::HashMap;

#[derive(Default)]
struct Catalogue {
    logins: usize,
    renewals: usize,
    categories: Vec<Value>,
    expire_first_token: bool,
    stall: bool,
    auth_quota: bool,
    top_prefix: Option<usize>,
    top_total: Option<usize>,
    missing_page_info: bool,
}

fn song(index: usize) -> Value {
    json!({"SNG_ID":index+10,"SNG_TITLE":format!("Song {index}"),"ISRC":format!("FIXTURE{index:05}"),
        "ART_ID":2,"ART_NAME":"Artist","ALB_ID":3,"ALB_TITLE":"Album"})
}

async fn gateway(
    State(state): State<Arc<Mutex<Catalogue>>>,
    Query(query): Query<HashMap<String, String>>,
    Json(args): Json<Value>,
) -> Json<Value> {
    let results = match query["method"].as_str() {
        "deezer.getUserData" => json!({"checkForm":"fixture-form","USER":{"USER_ID":9000}}),
        "deezer.pageArtist" => {
            assert_eq!(args, json!({"art_id":"2","lang":"en","tab":0}));
            let state = state.lock().unwrap();
            let count = state.top_prefix.unwrap_or(9);
            let total = state.top_total.unwrap_or(9);
            json!({"DATA":{"ART_ID":2,"ART_NAME":"Artist"},
                "TOP":{"data":(0..count).map(song).collect::<Vec<_>>(),"total":total},
                "HIGHLIGHT":{"TYPE":"album","ITEM":{"ALB_ID":3,"ALB_TITLE":"Latest","ART_ID":2,"ART_NAME":"Artist","SONGS":{"data":[song(0),song(1)]}}},
                "RELATED_ARTISTS":{"data":[{"ART_ID":4,"ART_NAME":"Related"}]},
                "RELATED_PLAYLIST":{"data":[{"PLAYLIST_ID":5,"TITLE":"Curated"}]}})
        }
        "artist.getTopTrack" => {
            let start = args["start"].as_u64().unwrap() as usize;
            assert_eq!(args["nb"], 30);
            let total = state.lock().unwrap().top_total.unwrap_or(9);
            json!({"data":(start..total).map(song).take(30).collect::<Vec<_>>(),"total":total})
        }
        method => panic!("Unexpected gateway method {method}"),
    };
    Json(json!({"error":[],"results":results}))
}

async fn authenticate(
    State(state): State<Arc<Mutex<Catalogue>>>,
    Path(method): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    Json(args): Json<Value>,
) -> Response {
    assert_eq!(
        query,
        HashMap::from([
            ("jo".into(), "p".into()),
            ("rto".into(), "p".into()),
            ("i".into(), "p".into())
        ])
    );
    let mut state = state.lock().unwrap();
    if state.auth_quota {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", "12")],
            Json(json!({"error":"Quota"})),
        )
            .into_response();
    }
    match method.as_str() {
        "arl" => {
            assert_eq!(args["arl"], "a".repeat(64));
            state.logins += 1;
        }
        "renew" => {
            assert_eq!(args["refresh_token"], "fixture-refresh");
            state.renewals += 1;
        }
        _ => panic!("Unexpected authentication method"),
    }
    Json(json!({"jwt":format!("fixture-jwt-{}",state.logins+state.renewals),"refresh_token":"fixture-refresh"})).into_response()
}

async fn graphql(
    State(state): State<Arc<Mutex<Catalogue>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(body["operationName"], "ArtistDiscographyByType");
    assert_eq!(headers["accept-language"], "en");
    let authorization = headers["authorization"].to_str().unwrap();
    let mut state = state.lock().unwrap();
    if state.expire_first_token && authorization == "Bearer fixture-jwt-1" {
        return Json(json!({"errors":[{"type":"JwtTokenExpiredError","message":"Expired"}]}));
    }
    assert!(authorization.starts_with("Bearer fixture-jwt-"));
    let variables = &body["variables"];
    state.categories.push(variables.clone());
    assert_eq!(variables["artistId"], "2");
    assert_eq!(variables["nb"], 30);
    assert_eq!(variables["order"], "RANK");
    let featured = variables["roles"][0] == "FEATURED";
    if featured {
        assert_eq!(variables["mode"], "ALL");
    } else {
        assert_eq!(variables["mode"], "OFFICIAL");
    }
    let (base, label, length) = if featured {
        (600, "Featured", 1)
    } else if variables["types"][0] == "EP" {
        (200, "EP", 1)
    } else if variables["types"][0] == "SINGLES" {
        (300, "Single", 1)
    } else if variables["subType"] == "LIVE" {
        (400, "Live", 1)
    } else if variables["subType"] == "COMPILATION" {
        (500, "Compilation", 1)
    } else {
        assert_eq!(variables["subType"], "STUDIO");
        (100, "Studio", 35)
    };
    let offset = if variables["cursor"].is_null() {
        0
    } else {
        assert_eq!(variables["cursor"], "cursor:30/==");
        30
    };
    let edges: Vec<_> = if state.stall {
        vec![]
    } else {
        (offset..length).take(30).map(|index|json!({"node":{"id":format!("{}",base+index),
            "displayTitle":format!("{label} {index}"),"releaseDate":"2026-01-01","isFavorite":true,
            "cover":{"large":["https://cdn-images.dzcdn.net/images/cover/abc/500x500.jpg"]},
            "contributors":{"edges":[{"roles":["MAIN"],"node":{"id":if featured {"999"} else {"2"},"name":if featured {"Other artist"} else {"Artist"}}}]}}})).collect()
    };
    let mut response = json!({"data":{"artist":{"id":"2","albums":{"edges":edges,
        "pageInfo":{"hasNextPage":state.stall || offset+30<length,"endCursor":"cursor:30/=="}}}}});
    if state.missing_page_info {
        response["data"]["artist"]["albums"]["pageInfo"] = json!({});
    }
    Json(response)
}

async fn setup() -> (
    Adapter,
    Arc<Mutex<Catalogue>>,
    tokio::task::JoinHandle<()>,
    PathBuf,
) {
    let state = Arc::new(Mutex::new(Catalogue::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/ajax/gw-light.php", post(gateway))
        .route("/login/{method}", post(authenticate))
        .route("/api", post(graphql))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let adapter = Adapter::new(Endpoints::new(
        Some(&endpoint),
        Some(&endpoint),
        Some(&endpoint),
    ));
    let data = std::env::temp_dir().join(format!("deezer-artist-{}", uuid::Uuid::new_v4()));
    adapter.handle("initialize",json!({"protocol":1,"data_dir":data,"cache_dir":data.join("cache"),"locale":"en","network":{"enabled":true}})).await.unwrap();
    let client = adapter.client().with_arl(&"a".repeat(64));
    *adapter.client.lock().unwrap() = client;
    (adapter, state, server, data)
}

#[tokio::test]
async fn artist_pages_keep_provider_categories_and_only_load_the_first_page() {
    let (adapter, state, server, data) = setup().await;
    let page = adapter
        .handle("page.get", json!({"ref":"artist:2"}))
        .await
        .unwrap();
    assert_eq!(page["header"]["ref"], "artist:2");
    // The integration probe feeds this generated document to the native page shaper.
    if let Some(path) = std::env::var_os("NATIVE_ARTIST_FIXTURE") {
        std::fs::write(path, serde_json::to_vec_pretty(&page).unwrap()).unwrap();
    }
    let shelves = page["shelves"].as_array().unwrap();
    let ids: Vec<_> = shelves.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "tracks",
            "latest",
            "album",
            "eps",
            "single-releases",
            "live-albums",
            "compilation-releases",
            "appears",
            "similar",
            "playlists"
        ]
    );
    assert_eq!(shelves[0]["items"].as_array().unwrap().len(), 9);
    assert_eq!(shelves[0]["preview"], 5);
    assert!(shelves[0].get("more").is_none());
    assert_eq!(shelves[0]["items"][0]["isrc"], "FIXTURE00000");
    assert_eq!(shelves[1]["extra_tracks"].as_array().unwrap().len(), 2);
    assert_eq!(shelves[2]["items"].as_array().unwrap().len(), 30);
    assert_eq!(
        shelves[2]["more"],
        "artist-more:2:studio-albums:cursor:30/=="
    );
    assert_eq!(shelves[7]["items"][0]["artist_ref"], "artist:999");
    assert_eq!(state.lock().unwrap().logins, 1);
    assert_eq!(state.lock().unwrap().categories.len(), 6);
    let repeated = adapter
        .handle("page.get", json!({"ref":"artist:2","refresh":true}))
        .await
        .unwrap();
    assert_eq!(page, repeated, "Equal refresh must keep section and item identities");
    assert_eq!(state.lock().unwrap().categories.len(), 12);
    let more = adapter
        .handle("page.get", json!({"ref":shelves[2]["more"]}))
        .await
        .unwrap();
    assert_eq!(more["items"].as_array().unwrap().len(), 5);
    assert_eq!(more["items"][0]["ref"], "album:130");
    assert!(more["next"].is_null());
    assert_eq!(state.lock().unwrap().logins, 1);
    let top = adapter
        .handle("page.get", json!({"ref":"artist-more:2:top:5"}))
        .await
        .unwrap();
    assert_eq!(top["items"].as_array().unwrap().len(), 4);
    assert_eq!(top["items"][0]["ref"], "track:15");
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn simultaneous_catalogue_reads_share_token_renewal() {
    let (adapter, state, server, data) = setup().await;
    state.lock().unwrap().expire_first_token = true;
    let page = adapter
        .handle("page.get", json!({"ref":"artist:2"}))
        .await
        .unwrap();
    assert_eq!(page["header"]["ref"], "artist:2");
    assert_eq!(state.lock().unwrap().logins, 1);
    assert_eq!(state.lock().unwrap().renewals, 1);
    assert_eq!(state.lock().unwrap().categories.len(), 6);
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn popular_tracks_keep_a_bounded_loaded_list_and_a_remote_continuation() {
    let (adapter, state, server, data) = setup().await;
    {
        let mut state = state.lock().unwrap();
        state.top_prefix = Some(205);
        state.top_total = Some(205);
    }
    let page = adapter
        .handle("page.get", json!({"ref":"artist:2"}))
        .await
        .unwrap();
    let popular = &page["shelves"][0];
    assert_eq!(popular["id"], "tracks");
    assert_eq!(popular["preview"], 5);
    assert_eq!(popular["items"].as_array().unwrap().len(), 200);
    assert_eq!(popular["more"], "artist-more:2:top:200");
    let more = adapter
        .handle("page.get", json!({"ref":popular["more"]}))
        .await
        .unwrap();
    assert_eq!(more["items"].as_array().unwrap().len(), 5);
    assert_eq!(more["items"][0]["ref"], "track:210");
    assert!(more.get("next").is_none());
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn a_non_advancing_category_remains_an_error_and_recovers_on_refresh() {
    let (adapter, state, server, data) = setup().await;
    state.lock().unwrap().stall = true;
    assert!(adapter
        .handle("page.get", json!({"ref":"artist:2"}))
        .await
        .is_err());
    state.lock().unwrap().stall = false;
    assert!(adapter
        .handle("page.get", json!({"ref":"artist:2","refresh":true}))
        .await
        .is_ok());
    let count = state.lock().unwrap().categories.len();
    for target in [
        "artist-more:2:unknown:abc",
        "artist-more:2:studio-albums:",
        "artist-more:2:top:-1",
    ] {
        assert_eq!(
            adapter
                .handle("page.get", json!({"ref":target}))
                .await
                .unwrap_err()
                .code,
            -32602
        );
    }
    assert_eq!(state.lock().unwrap().categories.len(), count);
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn authentication_quota_preserves_a_safe_retry_delay() {
    let (adapter, state, server, data) = setup().await;
    state.lock().unwrap().auth_quota = true;
    let error = adapter
        .handle("page.get", json!({"ref":"artist:2"}))
        .await
        .unwrap_err();
    let safe = adapter.safe_error(error);
    assert_eq!(safe.code, -32004);
    assert_eq!(safe.to_json()["data"], json!({"retry_after":12}));
    assert!(state.lock().unwrap().categories.is_empty());
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn a_short_top_preview_continues_after_the_rows_actually_shown() {
    let (adapter, state, server, data) = setup().await;
    state.lock().unwrap().top_prefix = Some(3);
    let page = adapter
        .handle("page.get", json!({"ref":"artist:2"}))
        .await
        .unwrap();
    assert_eq!(page["shelves"][0]["more"], "artist-more:2:top:3");
    let more = adapter
        .handle("page.get", json!({"ref":page["shelves"][0]["more"]}))
        .await
        .unwrap();
    assert_eq!(more["items"][0]["ref"], "track:13");
    assert_eq!(more["items"].as_array().unwrap().len(), 6);
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}

#[tokio::test]
async fn missing_pagination_state_is_not_reported_as_a_complete_discography() {
    let (adapter, state, server, data) = setup().await;
    state.lock().unwrap().missing_page_info = true;
    assert!(adapter
        .handle("page.get", json!({"ref":"artist:2"}))
        .await
        .is_err());
    server.abort();
    std::fs::remove_dir_all(data).unwrap();
}
