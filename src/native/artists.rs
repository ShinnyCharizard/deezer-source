use super::{
    catalog::{item, reference},
    Adapter,
};
use crate::{
    deezer::{num, Deezer},
    plugin::{
        items,
        rpc::{RpcError, RpcResult},
    },
};
use futures_util::{stream, StreamExt, TryStreamExt};
use serde_json::{json, Value};

const PAGE_SIZE: usize = 30;
// The gateway already includes these rows; the artist view folds them to five.
const TOP_TRACK_LIMIT: usize = 200;
const DISCOGRAPHY: &str = r#"
query ArtistDiscographyByType($artistId: String!, $nb: Int!, $roles: [ContributorRoles!]!,
 $types: [AlbumTypeInput!]!, $subType: AlbumSubTypeInput, $mode: DiscographyMode,
 $cursor: String, $order: AlbumOrder) {
 artist(artistId: $artistId) { id
  albums(after: $cursor, first: $nb, onlyCanonical: true, roles: $roles,
   types: $types, subType: $subType, mode: $mode, order: $order) {
   edges { node { id displayTitle releaseDate isFavorite isExplicit
    cover { large: urls(pictureRequest: {width: 500, height: 500}) }
    contributors { edges { roles node { ... on Artist { id name } } } }
   } }
   pageInfo { hasNextPage endCursor }
  }
 }
}"#;

#[derive(Clone, Copy)]
struct Category {
    // Continuation routes remain stable when presentation IDs change.
    id: &'static str,
    // Separate release categories retain their own labels and relative order.
    shelf_id: &'static str,
    english: &'static str,
    spanish: &'static str,
    types: &'static [&'static str],
    subtype: Option<&'static str>,
    role: &'static str,
    mode: &'static str,
}

const CATEGORIES: [Category; 6] = [
    Category {
        id: "studio-albums",
        shelf_id: "album",
        english: "Albums",
        spanish: "Álbumes",
        types: &["ALBUM"],
        subtype: Some("STUDIO"),
        role: "MAIN",
        mode: "OFFICIAL",
    },
    Category {
        id: "eps",
        shelf_id: "eps",
        english: "EPs",
        spanish: "EPs",
        types: &["EP"],
        subtype: None,
        role: "MAIN",
        mode: "OFFICIAL",
    },
    Category {
        id: "singles",
        shelf_id: "single-releases",
        english: "Singles",
        spanish: "Sencillos",
        types: &["SINGLES"],
        subtype: None,
        role: "MAIN",
        mode: "OFFICIAL",
    },
    Category {
        id: "live-albums",
        shelf_id: "live-albums",
        english: "Live albums",
        spanish: "Álbumes en vivo",
        types: &["ALBUM"],
        subtype: Some("LIVE"),
        role: "MAIN",
        mode: "OFFICIAL",
    },
    Category {
        id: "compilations",
        shelf_id: "compilation-releases",
        english: "Compilations",
        spanish: "Recopilaciones",
        types: &["ALBUM"],
        subtype: Some("COMPILATION"),
        role: "MAIN",
        mode: "OFFICIAL",
    },
    Category {
        id: "appears-on",
        shelf_id: "appears",
        english: "Featured in",
        spanish: "Aparece en",
        types: &["ALBUM", "EP", "SINGLES"],
        subtype: None,
        role: "FEATURED",
        mode: "ALL",
    },
];

fn rows(value: &Value) -> Result<&[Value], RpcError> {
    value["data"]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| RpcError::unavailable("The artist section is missing"))
}

fn gateway_items(values: &[Value], kind: &str, limit: usize) -> Result<Vec<Value>, RpcError> {
    values
        .iter()
        .take(limit)
        .map(|value| {
            let converted = match kind {
                "track" => items::track(value, None),
                "artist" => items::artist(value),
                "playlist" => items::playlist(value),
                _ => None,
            }
            .ok_or_else(|| RpcError::unavailable("The artist section contains an invalid item"))?;
            item(converted.to_json())
        })
        .collect()
}

fn graphql_album(node: &Value) -> RpcResult {
    if node["displayTitle"].as_str().is_none_or(str::is_empty) {
        return Err(RpcError::unavailable("The album title is missing"));
    }
    let artist = node["contributors"]["edges"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|edge| {
            edge["roles"]
                .as_array()
                .is_some_and(|roles| roles.iter().any(|role| role == "MAIN"))
        })
        .map(|edge| &edge["node"]);
    let raw = json!({"id":node["id"],"title":node["displayTitle"],
        "artist":artist,"release_date":node["releaseDate"],
        "cover_xl":node["cover"]["large"][0],"explicit_lyrics":node["isExplicit"]});
    let album = items::album(&raw, None)
        .ok_or_else(|| RpcError::unavailable("The album identity is missing"))?;
    let mut album = item(album.to_json())?;
    if node["isFavorite"].is_boolean() {
        album["favorite"] = node["isFavorite"].clone();
    }
    Ok(album)
}

async fn category_page(
    client: &Deezer,
    id: &str,
    category: &Category,
    cursor: Option<&str>,
) -> RpcResult {
    let data = client
        .graphql(
            "ArtistDiscographyByType",
            DISCOGRAPHY,
            json!({
                "artistId":id,"nb":PAGE_SIZE,"cursor":cursor,"subType":category.subtype,
                "roles":[category.role],"mode":category.mode,"order":"RANK","types":category.types
            }),
        )
        .await?;
    if data["artist"]["id"].as_str() != Some(id) {
        return Err(RpcError::unavailable("The artist identity changed"));
    }
    let albums = &data["artist"]["albums"];
    let edges = albums["edges"]
        .as_array()
        .filter(|edges| edges.len() <= PAGE_SIZE)
        .ok_or_else(|| RpcError::unavailable("The artist discography page is invalid"))?;
    let values = edges
        .iter()
        .map(|edge| graphql_album(&edge["node"]))
        .collect::<Result<Vec<_>, _>>()?;
    let mut page = json!({"items":values,"sortable":true});
    let has_next = albums["pageInfo"]["hasNextPage"]
        .as_bool()
        .ok_or_else(|| RpcError::unavailable("The discography pagination state is missing"))?;
    if has_next {
        let next = albums["pageInfo"]["endCursor"]
            .as_str()
            .filter(|next| {
                !next.is_empty()
                    && next.len() <= 2048
                    && Some(*next) != cursor
                    && !next.chars().any(char::is_control)
            })
            .ok_or_else(|| RpcError::unavailable("The discography continuation did not advance"))?;
        if edges.is_empty() {
            return Err(RpcError::unavailable(
                "The discography page did not advance",
            ));
        }
        page["next"] = json!(format!("artist-more:{id}:{}:{next}", category.id));
    }
    Ok(page)
}

impl Adapter {
    pub(super) async fn artist_page(&self, id: &str) -> RpcResult {
        self.authenticated().await?;
        let client = self.client();
        let page = client
            .gw(
                "deezer.pageArtist",
                json!({"art_id":id,"lang":client.language(),"tab":0}),
            )
            .await?;
        let artist = items::artist(&page["DATA"])
            .ok_or_else(|| RpcError::unavailable("The artist header is missing"))?;
        let mut header = item(artist.to_json())?;
        if header["ref"] != format!("artist:{id}") {
            return Err(RpcError::unavailable("The artist identity changed"));
        }
        if let Some(bio) = page["BIO"]["RESUME"].as_str().filter(|bio| !bio.is_empty()) {
            header["description"] = json!(bio.chars().take(4000).collect::<String>());
        }
        let top = gateway_items(rows(&page["TOP"])?, "track", TOP_TRACK_LIMIT)?;
        let shown = top.len() as u64;
        let top_total = num(page["TOP"].get("total")).unwrap_or(rows(&page["TOP"])?.len() as u64);
        if shown == 0 && top_total > 0 {
            return Err(RpcError::unavailable(
                "The artist top tracks did not advance",
            ));
        }
        let mut top_shelf = json!({"id":"tracks","title":self.text("Popular Tracks","Pistas populares"),
            "kind":"track","preview":5,"items":top,"previous_ids":["top-songs"]});
        if top_total > shown {
            top_shelf["more"] = json!(format!("artist-more:{id}:top:{shown}"));
        }
        let mut shelves = vec![top_shelf];
        if page["HIGHLIGHT"]["TYPE"] == "album" {
            let raw = &page["HIGHLIGHT"]["ITEM"];
            let album = items::album(raw, None)
                .ok_or_else(|| RpcError::unavailable("The highlighted album is invalid"))?;
            let tracks = raw["SONGS"]["data"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            shelves.push(json!({"id":"latest","title":self.text("Latest release","Último lanzamiento"),
                "kind":"album","items":[item(album.to_json())?],"extra_tracks":gateway_items(tracks,"track",6)?,
                "previous_ids":["latest-release"]}));
        }
        // Thirty cards per category, two requests in flight, no retained page cache.
        let listings: Vec<Value> = stream::iter(CATEGORIES.into_iter().map(|category| {
            let client = client.clone();
            let id = id.to_owned();
            async move { category_page(&client, &id, &category, None).await }
        }))
        .buffered(2)
        .try_collect()
        .await?;
        for (category, listing) in CATEGORIES.iter().zip(listings) {
            if listing["items"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
            {
                let mut shelf = json!({"id":category.shelf_id,"title":self.text(category.english,category.spanish),
                    "kind":"album","items":listing["items"],"sortable":true});
                // The new key isolates studio rows from the old mixed-release
                // cache; aliases retain the section's presentation preferences.
                if category.id == "studio-albums" {
                    shelf["previous_ids"] = json!(["albums", "studio-albums"]);
                } else if category.id != category.shelf_id {
                    shelf["previous_ids"] = json!([category.id]);
                }
                if listing["next"].is_string() {
                    shelf["more"] = listing["next"].clone();
                }
                shelves.push(shelf);
            }
        }
        for (field, section, english, spanish, kind, limit) in [
            (
                "RELATED_ARTISTS",
                "similar",
                "Similar artists",
                "Artistas similares",
                "artist",
                6,
            ),
            (
                "RELATED_PLAYLIST",
                "playlists",
                "Playlists",
                "Playlists",
                "playlist",
                6,
            ),
        ] {
            if let Some(values) = page[field]["data"]
                .as_array()
                .filter(|values| !values.is_empty())
            {
                shelves.push(
                    json!({"id":section,"title":self.text(english,spanish),"kind":kind,
                    "items":gateway_items(values,kind,limit)?}),
                );
            }
        }
        Ok(json!({"title":header["title"],"header":header,"shelves":shelves}))
    }

    pub(super) async fn artist_more(&self, target: &str) -> RpcResult {
        let mut parts = target.splitn(4, ':');
        let (_, id, section, cursor) = (
            parts.next(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        );
        reference(&json!(format!("artist:{id}")), &["artist"])?;
        if cursor.is_empty() || cursor.len() > 2048 || cursor.chars().any(char::is_control) {
            return Err(RpcError::invalid_params("Invalid artist continuation"));
        }
        if section == "top" {
            let offset: u64 = cursor
                .parse()
                .ok()
                .filter(|offset| *offset <= 10000)
                .ok_or_else(|| RpcError::invalid_params("Invalid top-track offset"))?;
            self.authenticated().await?;
            let page = self
                .client()
                .gw(
                    "artist.getTopTrack",
                    json!({"art_id":id,"start":offset,"nb":PAGE_SIZE}),
                )
                .await?;
            let raw = rows(&page)?;
            if raw.len() > PAGE_SIZE {
                return Err(RpcError::unavailable(
                    "The top-track page ignored its limit",
                ));
            }
            let values = gateway_items(raw, "track", PAGE_SIZE)?;
            let more =
                num(page.get("total")).is_some_and(|total| offset + (raw.len() as u64) < total);
            if more && raw.is_empty() {
                return Err(RpcError::unavailable("The top-track page did not advance"));
            }
            let mut listing =
                json!({"title":self.text("Top tracks","Pistas populares"),"items":values});
            if more {
                listing["next"] = json!(format!(
                    "artist-more:{id}:top:{}",
                    offset + raw.len() as u64
                ));
            }
            return Ok(listing);
        }
        let category = CATEGORIES
            .iter()
            .find(|category| category.id == section)
            .ok_or_else(|| RpcError::invalid_params("Unknown discography category"))?;
        self.authenticated().await?;
        let mut listing = category_page(&self.client(), id, category, Some(cursor)).await?;
        listing["title"] = json!(self.text(category.english, category.spanish));
        Ok(listing)
    }
}
