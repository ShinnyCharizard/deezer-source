//! Account-owned playlist writes use the web gateway, without automatic retries.
use super::{catalog::reference, Adapter};
use crate::{
    deezer::num,
    plugin::{
        catalog::Tab,
        items,
        rpc::{RpcError, RpcResult},
    },
};
use serde_json::{json, Value};

fn metadata(params: &Value) -> RpcResult {
    let title = params["title"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= 200)
        .ok_or_else(|| RpcError::invalid_params("Invalid playlist title"))?;
    let description = match params.get("description") {
        None => "",
        Some(value) => value
            .as_str()
            .filter(|s| s.chars().count() <= 2000)
            .ok_or_else(|| RpcError::invalid_params("Invalid playlist description"))?,
    };
    let public = match params.get("public") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| RpcError::invalid_params("Invalid playlist visibility"))?,
    };
    Ok(json!({"title":title,"description":description,"status":if public {0} else {1}}))
}

fn songs(params: &Value) -> Result<Vec<Value>, RpcError> {
    let tracks = params["tracks"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 100)
        .ok_or_else(|| RpcError::invalid_params("Choose between 1 and 100 tracks"))?;
    tracks
        .iter()
        .map(|track| {
            let original = reference(track, &["track"])?;
            let id = original.strip_prefix("track/").unwrap_or_default();
            if id == "0" {
                return Err(RpcError::invalid_params("Invalid track identity"));
            }
            Ok(json!([id, 0]))
        })
        .collect()
}

fn created_id(value: &Value) -> Option<String> {
    let id = value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
        .or_else(|| items::id_at(value, "PLAYLIST_ID"))
        .or_else(|| items::id_at(value, "id"))?;
    (items::valid_id(&id) && !id.starts_with('-') && id != "0").then_some(id)
}

impl Adapter {
    async fn playlist_write(&self, method: &str, args: Value) -> RpcResult {
        self.client().gw_write(method, args).await.map_err(|error| {
            let error = RpcError::from(error);
            if matches!(error.code, -32005 | -32603) {
                RpcError::new(-32006, "Unconfirmed playlist write")
            } else {
                error
            }
        })
    }

    pub(super) async fn edit_playlist(&self, params: Value) -> RpcResult {
        let action = params["action"].as_str().unwrap_or_default();
        if !matches!(
            action,
            "playlist.create"
                | "playlist.add"
                | "playlist.update"
                | "playlist.delete"
                | "playlist.search"
        ) {
            return Err(RpcError::new(-32601, "Unsupported playlist action"));
        }
        let _guard = self.playlist_writes.lock().await;
        let user = self.authenticated().await?;
        if action == "playlist.search" {
            let results = self
                .search(json!({"text":params["text"],"limit":30}))
                .await?;
            return Ok(json!({"read_only":true,"items":results["tracks"]}));
        }
        let details = if matches!(action, "playlist.create" | "playlist.update") {
            Some(metadata(&params)?)
        } else {
            None
        };
        let tracks = if action == "playlist.add" {
            Some(songs(&params)?)
        } else {
            None
        };
        if action == "playlist.create" {
            let mut args = details.unwrap();
            args["songs"] = json!([]);
            let result = self.playlist_write("playlist.create", args.clone()).await?;
            // Do not fetch the new list here: a failed read must not hide its creation.
            self.library.forget(Some(Tab::Playlists));
            let id = created_id(&result)
                .ok_or_else(|| RpcError::new(-32006, "Missing created playlist identity"))?;
            let reference = format!("playlist:{id}");
            return Ok(
                json!({"changed_refs":[reference],"item":{"kind":"playlist","ref":reference,
                "title":args["title"],"description":args["description"],"public":args["status"]==0,
                "editable":true,"copyable":false,"tracks":0}}),
            );
        }
        let original = reference(&params["ref"], &["playlist"])?;
        let id = original.strip_prefix("playlist/").unwrap_or_default();
        let page = self
            .client()
            .gw("deezer.pagePlaylist", json!({"PLAYLIST_ID":id,"nb":0}))
            .await?;
        let header = &page["DATA"];
        if items::id_at(header, "PLAYLIST_ID").as_deref() != Some(id)
            || num(header.get("PARENT_USER_ID")) != Some(user.id)
        {
            return Err(RpcError::unavailable(
                "This playlist does not belong to the connected account",
            ));
        }
        let (method, mut args) = match action {
            "playlist.add" => (
                "playlist.addSongs",
                json!({"songs":tracks.unwrap(),"offset":-1}),
            ),
            "playlist.update" => ("playlist.update", details.unwrap()),
            "playlist.delete" => ("playlist.delete", json!({})),
            _ => unreachable!(),
        };
        args["PLAYLIST_ID"] = json!(id);
        let result = self.playlist_write(method, args).await?;
        self.library.forget(Some(Tab::Playlists));
        if result != true && !result.as_u64().is_some_and(|n| n > 0) {
            return Err(RpcError::new(
                -32006,
                "Missing playlist write acknowledgement",
            ));
        }
        Ok(json!({"changed_refs":[params["ref"]],"ref":params["ref"]}))
    }
}
