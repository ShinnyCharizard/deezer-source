//! Render supported music entities from the personalised web feed.
use super::{catalog::item, Adapter};
use crate::plugin::{
    items,
    rpc::{RpcError, RpcResult},
};
use serde_json::{json, Value};
use std::collections::HashSet;

// Per reply: 32 shelves with at most 24 preview cards each. Module listings
// retain no cache and expose 200 rows per page, from at most 1,000 entities.
const MAX_SHELVES: usize = 32;
const PREVIEW: usize = 24;
const MODULE_LIMIT: usize = 1000;
const PAGE_SIZE: usize = 200;

fn module_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn music(entry: &Value) -> Option<Value> {
    let data = entry.get("data").unwrap_or(entry);
    let kind = data["__TYPE__"]
        .as_str()
        .or_else(|| entry["type"].as_str())?;
    let raw = match kind {
        "album" => items::album(data, None),
        "artist" => items::artist(data),
        "playlist" => items::playlist(data),
        "song" | "track" => items::track(data, None),
        _ => None,
    }?;
    item(raw.to_json()).ok()
}

fn sections(page: &Value) -> Result<&Vec<Value>, RpcError> {
    page["sections"]
        .as_array()
        .ok_or_else(|| RpcError::unavailable("Unreadable discovery feed"))
}

impl Adapter {
    async fn feed(&self, target: &str) -> RpcResult {
        let language = self
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .language
            .clone();
        let language = language.split(['-', '_', '.']).next().unwrap_or("en");
        self.client()
            .gw(
                "page.get",
                json!({"PAGE":target,"VERSION":"2.5","LANG":language,"OPTIONS":[],
            "SUPPORT":{"grid":["album","artist","playlist","song"],
                "horizontal-grid":["album","artist","playlist","song"]}}),
            )
            .await
            .map_err(Into::into)
    }

    pub(super) async fn discovery_home(&self) -> RpcResult {
        let page = self.feed("home").await?;
        let mut shelves = Vec::new();
        let mut known = HashSet::new();
        for section in sections(&page)? {
            if shelves.len() == MAX_SHELVES {
                break;
            }
            // section_id embeds render_id and tracking data, so it cannot be a UI identity.
            let Some(id) = section["module_id"].as_str().filter(|s| module_id(s)) else {
                continue;
            };
            if !known.insert(id) {
                continue;
            }
            let Some(title) = section["title"].as_str().filter(|s| !s.trim().is_empty()) else {
                continue;
            };
            let entries: Vec<_> = section["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(music)
                .take(PREVIEW)
                .collect();
            if entries.is_empty() {
                continue;
            }
            let kind = entries[0]["kind"].as_str().unwrap_or("mixed");
            let kind = if entries.iter().all(|e| e["kind"] == kind) {
                kind
            } else {
                "mixed"
            };
            let mut shelf = json!({"id":format!("feed-{id}"),"title":title,"kind":kind,"items":entries,"sortable":kind!="track"});
            if kind == "mixed" {
                let (tracks, cards): (Vec<_>, Vec<_>) =
                    entries.into_iter().partition(|e| e["kind"] == "track");
                shelf["items"] = json!(cards);
                shelf["extra_tracks"] = json!(tracks);
            }
            if section["target"]
                .as_str()
                .is_some_and(|s| s.trim_start_matches('/') == format!("channels/module/{id}"))
            {
                shelf["more"] = json!(format!("feed:{id}:0"));
            }
            shelves.push(shelf);
        }
        if shelves.is_empty() {
            return Err(RpcError::unavailable(
                "The feed contains no supported music sections",
            ));
        }
        Ok(json!({"title":self.text("Home","Inicio"),"personal":true,"shelves":shelves}))
    }

    pub(super) async fn discovery_module(&self, reference: &str) -> RpcResult {
        let (id, offset) = reference
            .rsplit_once(':')
            .ok_or_else(|| RpcError::invalid_params("Invalid feed continuation"))?;
        let offset = offset
            .parse::<usize>()
            .ok()
            .filter(|n| *n <= MODULE_LIMIT)
            .ok_or_else(|| RpcError::invalid_params("Invalid feed offset"))?;
        if !module_id(id) {
            return Err(RpcError::invalid_params("Invalid feed module"));
        }
        self.authenticated().await?;
        let page = self.feed(&format!("channels/module/{id}")).await?;
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        for section in sections(&page)? {
            if section["hasMoreItems"] == true {
                return Err(RpcError::unavailable(
                    "The service returned an incomplete module",
                ));
            }
            for entry in section["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(music)
            {
                if seen.insert(entry["ref"].as_str().unwrap_or_default().to_owned()) {
                    entries.push(entry);
                    if entries.len() > MODULE_LIMIT {
                        return Err(RpcError::unavailable(
                            "The feed module exceeds the supported item limit",
                        ));
                    }
                }
            }
        }
        let end = (offset + PAGE_SIZE).min(entries.len());
        let mut result = json!({"title":page["title"].as_str().unwrap_or(&self.text("Discover","Descubrir")),
            "personal":true,"items":entries[offset.min(entries.len())..end],"sortable":true});
        if end < entries.len() {
            result["next"] = json!(format!("feed:{id}:{end}"));
        }
        Ok(result)
    }
}
