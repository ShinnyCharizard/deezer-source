//! Translate native catalogue documents without changing provider identities.
use super::Adapter;
use crate::plugin::{
    catalog, items,
    rpc::{RpcError, RpcResult},
};
use serde_json::{json, Value};

pub(super) fn reference(value: &Value, kinds: &[&str]) -> Result<String, RpcError> {
    let raw = value
        .as_str()
        .ok_or_else(|| RpcError::invalid_params("Missing reference"))?;
    let (kind, id) = raw
        .split_once(':')
        .ok_or_else(|| RpcError::invalid_params("Invalid reference"))?;
    if !kinds.contains(&kind) || !items::valid_id(id) {
        return Err(RpcError::invalid_params("Invalid reference"));
    }
    Ok(format!("{kind}/{id}"))
}

pub(super) fn item(raw: Value) -> Result<Value, RpcError> {
    let kind = raw["kind"].as_str().unwrap_or_default();
    let original = raw["ref"].as_str().unwrap_or_default();
    let (prefix, id) = original
        .split_once('/')
        .ok_or_else(|| RpcError::invalid_params("Invalid catalogue item"))?;
    if kind != prefix
        || !matches!(kind, "track" | "album" | "artist" | "playlist")
        || !items::valid_id(id)
    {
        return Err(RpcError::invalid_params("Invalid catalogue identity"));
    }
    let mut result = json!({"kind":kind,"ref":format!("{kind}:{id}")});
    for key in [
        "title",
        "artist",
        "album",
        "genre",
        "year",
        "duration_ms",
        "favorite",
        "release_date",
        "label",
        "isrc",
        "explicit",
        "added_at",
        "updated_at",
        "quality",
        "editable",
        "description",
        "public",
    ] {
        if !raw[key].is_null() {
            result[key] = raw[key].clone();
        }
    }
    for (from, to) in [
        ("track_no", "track_number"),
        ("disc_no", "disc_number"),
        ("track_count", "tracks"),
        ("art", "image"),
    ] {
        if !raw[from].is_null() {
            result[to] = raw[from].clone();
        }
    }
    for key in ["artist_ref", "album_ref"] {
        if let Some(reference) = raw[key].as_str() {
            if let Some((kind, id)) = reference.split_once('/') {
                if matches!(kind, "artist" | "album") && items::valid_id(id) {
                    result[key] = json!(format!("{kind}:{id}"));
                }
            }
        }
    }
    if raw["playable"] == false && kind == "track" {
        result["available"] = json!(false);
    }
    if kind == "playlist" {
        result["copyable"] = json!(false);
    }
    Ok(result)
}

fn entries(raw: &Value) -> Result<Vec<Value>, RpcError> {
    raw["items"]
        .as_array()
        .ok_or_else(|| RpcError::invalid_params("Invalid catalogue page"))?
        .iter()
        .cloned()
        .map(item)
        .collect()
}

pub(super) fn count(
    params: &Value,
    key: &str,
    default: u64,
    maximum: u64,
) -> Result<u64, RpcError> {
    match params.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|n| *n <= maximum)
            .ok_or_else(|| RpcError::invalid_params("Invalid page bounds")),
    }
}

impl Adapter {
    pub(super) async fn search(&self, params: Value) -> RpcResult {
        let text = params["text"]
            .as_str()
            .ok_or_else(|| RpcError::invalid_params("Missing search text"))?;
        if text.chars().count() > 200 {
            return Err(RpcError::invalid_params(
                "Search is limited to 200 characters",
            ));
        }
        let limit = count(&params, "limit", 20, 10000)?.min(50);
        let mut result = json!({"artists":[],"albums":[],"tracks":[],"playlists":[]});
        if text.trim().is_empty() || limit == 0 {
            return Ok(result);
        }
        let request = serde_json::from_value(json!({"query":text.trim(),"kinds":["track","album","artist"],"offset":0,"limit":limit}))
            .map_err(|_| RpcError::invalid_params("Invalid search"))?;
        let raw = catalog::search(&self.client(), request).await?;
        for group in raw["groups"].as_array().into_iter().flatten() {
            let key = match group["kind"].as_str() {
                Some("track") => "tracks",
                Some("artist") => "artists",
                Some("album") => "albums",
                _ => continue,
            };
            result[key] = json!(entries(group)?);
        }
        Ok(result)
    }

    async fn list(&self, original: &str, offset: u64, limit: u64) -> RpcResult {
        let request =
            serde_json::from_value(json!({"ref":original,"offset":offset,"limit":limit.min(200)}))
                .map_err(|_| RpcError::invalid_params("Invalid listing"))?;
        catalog::list(&self.client(), &self.library, request).await
    }

    pub(super) async fn get_item(&self, reference: &str) -> RpcResult {
        item(catalog::get(&self.client(), reference).await?)
    }

    pub(super) async fn collection(&self, params: Value, playlist: bool) -> RpcResult {
        if !playlist {
            return self.album(params).await;
        }
        let kind = if playlist { "playlist" } else { "album" };
        let original = reference(&params["ref"], &[kind])?;
        if playlist {
            self.authenticated().await?;
        }
        let header = self.get_item(&original).await?;
        let offset = count(&params, "offset", 0, 100000)?;
        let maximum = count(&params, "limit", 10000, 10000)?;
        let mut tracks = Vec::new();
        let mut more = false;
        while tracks.len() < maximum as usize {
            let page = self
                .list(
                    &original,
                    offset + tracks.len() as u64,
                    (maximum - tracks.len() as u64).min(200),
                )
                .await?;
            let values = entries(&page)?;
            more = page["has_more"].as_bool().unwrap_or(false);
            if values.is_empty() && more {
                return Err(RpcError::unavailable(
                    "The catalogue returned a non-advancing page",
                ));
            }
            tracks.extend(values);
            if !more {
                break;
            }
        }
        if more {
            return Err(RpcError::unavailable(
                "This collection exceeds the requested track limit",
            ));
        }
        Ok(json!({kind:header,"tracks":tracks}))
    }

    pub(super) async fn library_list(&self, params: Value) -> RpcResult {
        self.authenticated().await?;
        let kind = params["kind"].as_str().unwrap_or_default();
        let tab = match kind {
            "albums" => catalog::Tab::Albums,
            "artists" => catalog::Tab::Artists,
            "tracks" => catalog::Tab::Tracks,
            "playlists" => catalog::Tab::Playlists,
            _ => return Err(RpcError::new(-32601, "Unsupported library kind")),
        };
        let offset = count(&params, "offset", 0, 100000)?;
        let maximum = count(&params, "limit", 500, 10000)?.min(500);
        if params["refresh"] == true && offset == 0 {
            self.library.forget(Some(tab));
        }
        let mut values = Vec::new();
        let mut total = 0;
        while values.len() < maximum as usize {
            let request = serde_json::from_value(json!({"offset":offset+values.len() as u64,"limit":(maximum-values.len() as u64).min(200)}))
                .map_err(|_| RpcError::invalid_params("Invalid library bounds"))?;
            let page = catalog::library(
                &self.client(),
                &self.library,
                &format!("library.{kind}"),
                request,
            )
            .await?;
            let next = entries(&page)?;
            total = page["total"]
                .as_u64()
                .unwrap_or(offset + values.len() as u64 + next.len() as u64);
            let more = page["has_more"].as_bool().unwrap_or(false);
            if next.is_empty() && more {
                return Err(RpcError::unavailable(
                    "The library returned a non-advancing page",
                ));
            }
            values.extend(next);
            if !more {
                break;
            }
        }
        Ok(json!({"items":values,"total":total}))
    }

    async fn shelf(
        &self,
        id: &str,
        title: &str,
        kind: &str,
        original: &str,
        view_all: bool,
    ) -> RpcResult {
        let page = self.list(original, 0, 200).await?;
        let mut shelf = json!({"id":id,"title":title,"kind":kind,"items":entries(&page)?,"sortable":kind!="track"});
        // Home's See all opens a complete listing, including the preview.
        // An artist's expanding shelf continues after its loaded rows.
        if view_all {
            shelf["more"] = json!(format!("more:{original}:0"));
        } else if page["has_more"] == true {
            shelf["more"] = json!(format!("more:{original}:200"));
        }
        Ok(shelf)
    }

    pub(super) async fn page(&self, params: Value) -> RpcResult {
        let target = params["ref"].as_str().unwrap_or_default();
        if target.starts_with("artist-more:") {
            return self.artist_more(target).await;
        }
        if let Some(module) = target.strip_prefix("feed:") {
            return self.discovery_module(module).await;
        }
        if target == "discover:home" {
            if self.client().has_arl() {
                self.authenticated().await?;
                return self.discovery_home().await;
            }
            let releases_title = self.text("New releases", "Novedades");
            let albums_title = self.text("Popular albums", "Álbumes populares");
            let tracks_title = self.text("Popular tracks", "Pistas populares");
            let (releases, albums, tracks) = tokio::try_join!(
                self.shelf("releases", &releases_title, "album", "releases", true),
                self.shelf(
                    "popular-albums",
                    &albums_title,
                    "album",
                    "charts/albums",
                    true
                ),
                self.shelf(
                    "popular-tracks",
                    &tracks_title,
                    "track",
                    "charts/tracks",
                    true
                )
            )?;
            return Ok(
                json!({"title":self.text("Home","Inicio"),"shelves":[releases,albums,tracks]}),
            );
        }
        if let Some(more) = target.strip_prefix("more:") {
            let (original, offset) = more
                .rsplit_once(':')
                .ok_or_else(|| RpcError::invalid_params("Invalid continuation"))?;
            let offset: u64 = offset
                .parse()
                .ok()
                .filter(|n| *n <= 100000)
                .ok_or_else(|| RpcError::invalid_params("Invalid continuation offset"))?;
            let parsed = items::Ref::parse(original)
                .ok_or_else(|| RpcError::invalid_params("Unknown continuation"))?;
            if !matches!(
                parsed,
                items::Ref::Artist(_)
                    | items::Ref::Album(_)
                    | items::Ref::Releases
                    | items::Ref::Chart(_)
            ) {
                return Err(RpcError::invalid_params("Unsupported continuation"));
            }
            let page = self.list(original, offset, 200).await?;
            let values = entries(&page)?;
            if page["has_more"] == true && values.is_empty() {
                return Err(RpcError::unavailable("The continuation did not advance"));
            }
            let title = match original {
                "releases" => self.text("New releases", "Novedades"),
                "charts/albums" => self.text("Popular albums", "Álbumes populares"),
                "charts/tracks" => self.text("Popular tracks", "Pistas populares"),
                _ => self.text("Catalogue", "Catálogo"),
            };
            let mut result = json!({"title":title,"items":values,"sortable":true});
            if page["has_more"] == true {
                result["next"] = json!(format!("more:{original}:{}", offset + values.len() as u64));
            }
            return Ok(result);
        }
        let original = reference(&params["ref"], &["artist", "album", "playlist"])?;
        if original.starts_with("playlist/") || original.starts_with("album/") {
            let playlist = original.starts_with("playlist/");
            let kind = if playlist { "playlist" } else { "album" };
            let body = self.collection(json!({"ref":target}), playlist).await?;
            return Ok(
                json!({"title":body[kind]["title"],"header":body[kind],"shelves":[{"id":"tracks","title":self.text("Tracks","Pistas"),"kind":"track","items":body["tracks"]}]}),
            );
        }
        if self.client().has_arl() {
            return self.artist_page(original.trim_start_matches("artist/")).await;
        }
        let header = self.get_item(&original).await?;
        let (id, kind, title) = if original.starts_with("artist/") {
            ("albums", "album", self.text("Albums", "Álbumes"))
        } else {
            ("tracks", "track", self.text("Tracks", "Pistas"))
        };
        let shelf = self.shelf(id, &title, kind, &original, false).await?;
        Ok(json!({"title":header["title"],"header":header,"shelves":[shelf]}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_and_sort_metadata_survive_translation() {
        let value=item(json!({"kind":"album","ref":"album/3","title":"Beta","artist":"A","release_date":"2024-01-01","added_at":100,"art":"https://cdn-images.dzcdn.net/a.jpg"})).unwrap();
        assert_eq!(value["ref"], "album:3");
        assert_eq!(value["release_date"], "2024-01-01");
        assert_eq!(value["added_at"], 100);
        assert!(!value["image"].is_null());
        assert!(reference(&json!("album:3/../1"), &["album"]).is_err());
        assert!(reference(&json!("artist:3"), &["track"]).is_err());
    }
}
