use super::{
    catalog::{count, item, reference},
    Adapter,
};
use crate::{
    deezer::num,
    plugin::{
        items,
        rpc::{RpcError, RpcResult},
    },
};
use serde_json::{json, Value};

fn album_header(raw: &Value, expected: &str) -> RpcResult {
    let album = items::album(raw, None)
        .ok_or_else(|| RpcError::unavailable("The album header is missing"))?;
    if album.reference != expected {
        return Err(RpcError::unavailable("The album identity changed"));
    }
    item(album.to_json())
}

fn native_track(raw: &Value, album: &Value, context: &Value) -> RpcResult {
    let track = items::track(raw, Some(context))
        .ok_or_else(|| RpcError::unavailable("The album contains an invalid track"))?;
    let mut track = item(track.to_json())?;
    for key in [
        "artist",
        "artist_ref",
        "year",
        "release_date",
        "label",
        "genre",
        "image",
    ] {
        if track[key].is_null() && !album[key].is_null() {
            track[key] = album[key].clone();
        }
    }
    if track["album"].is_null() {
        track["album"] = album["title"].clone();
    }
    if track["album_ref"].is_null() {
        track["album_ref"] = album["ref"].clone();
    }
    Ok(track)
}

impl Adapter {
    pub(super) async fn album(&self, params: Value) -> RpcResult {
        let original = reference(&params["ref"], &["album"])?;
        let offset = count(&params, "offset", 0, 100000)?;
        let maximum = count(&params, "limit", 10000, 10000)?;
        let id = original.trim_start_matches("album/");
        let client = self.client();
        // Gateway replies contain the header and songs together. Nothing is cached
        // here, and only the host's bounded collection survives this request.
        let (header, embedded, total) = if client.has_arl() {
            let page = client
                .gw(
                    "deezer.pageAlbum",
                    json!({
                        "alb_id":id,"lang":client.language(),"tab":0,"header":true
                    }),
                )
                .await?;
            let header = album_header(&page["DATA"], &original)?;
            let songs = page["SONGS"]["data"]
                .as_array()
                .ok_or_else(|| RpcError::unavailable("The album songs are missing"))?;
            let total = num(page["SONGS"].get("total")).or_else(|| header["tracks"].as_u64());
            if total.is_some_and(|total| songs.len() as u64 > total) {
                return Err(RpcError::unavailable(
                    "The album track count is inconsistent",
                ));
            }
            let tracks = songs
                .iter()
                .skip(offset as usize)
                .take(maximum as usize)
                .map(|song| native_track(song, &header, &Value::Null))
                .collect::<Result<Vec<_>, _>>()?;
            (header, tracks, total)
        } else {
            let raw = client.api(&original, vec![]).await?;
            let header = album_header(&raw, &original)?;
            let total = header["tracks"].as_u64();
            (header, Vec::new(), total)
        };
        let artist_id = header["artist_ref"]
            .as_str()
            .and_then(|s| s.strip_prefix("artist:"));
        let context = json!({"id":id,"title":header["title"],
            "artist":{"id":artist_id,"name":header["artist"]},
            "release_date":header["release_date"],"label":header["label"]});
        let mut tracks = embedded;
        let mut more = total.is_none_or(|total| offset + (tracks.len() as u64) < total);
        while more && tracks.len() < maximum as usize {
            let start = offset + tracks.len() as u64;
            let limit = (maximum - tracks.len() as u64).min(200);
            let page = if client.has_arl() {
                client
                    .gw(
                        "song.getListByAlbum",
                        json!({"alb_id":id,"start":start,"nb":limit}),
                    )
                    .await?
            } else {
                client
                    .api(
                        format!("{original}/tracks"),
                        vec![("index", start.to_string()), ("limit", limit.to_string())],
                    )
                    .await?
            };
            let songs = page["data"]
                .as_array()
                .ok_or_else(|| RpcError::unavailable("The album track page is missing"))?;
            if songs.len() > limit as usize {
                return Err(RpcError::unavailable(
                    "The album ignored the requested page size",
                ));
            }
            let page_total = num(page.get("total")).or(total);
            more = page_total
                .map(|total| start + (songs.len() as u64) < total)
                .unwrap_or_else(|| page["next"].is_string());
            if songs.is_empty() && more {
                return Err(RpcError::unavailable(
                    "The album track page did not advance",
                ));
            }
            for song in songs {
                tracks.push(native_track(song, &header, &context)?);
            }
        }
        if more && maximum > 0 {
            return Err(RpcError::unavailable(
                "This album exceeds the requested track limit",
            ));
        }
        Ok(json!({"album":header,"tracks":tracks}))
    }
}
