//! A single native process owns account state, catalogue reads and the audio relay.
mod catalog;
#[cfg(test)]
mod tests;
mod transport;
mod wizard;

use crate::{
    account::{extract_arl, Credentials},
    deezer::{Deezer, Endpoints, SignedOut, User},
    plugin::{
        catalog as upstream, items,
        relay::Relay,
        resolve,
        rpc::{RpcError, RpcResult},
        settings::Quality,
    },
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use wizard::{Screen, Wizard};

pub(crate) use transport::run;

#[derive(Default)]
struct Config {
    data: PathBuf,
    language: String,
}

struct Adapter {
    config: Mutex<Config>,
    client: Mutex<Deezer>,
    wizard: AsyncMutex<Wizard>,
    operations: RwLock<()>,
    initialized: AtomicBool,
    online: AtomicBool,
    library: upstream::Library,
    relay: Mutex<Relay>,
}

impl Adapter {
    fn new(endpoints: Endpoints) -> Self {
        Self {
            config: Mutex::new(Config::default()),
            client: Mutex::new(Deezer::new(endpoints)),
            wizard: AsyncMutex::new(Wizard::default()),
            operations: RwLock::new(()),
            initialized: AtomicBool::new(false),
            online: AtomicBool::new(false),
            library: upstream::Library::default(),
            relay: Mutex::new(Relay::default()),
        }
    }
    fn client(&self) -> Deezer {
        self.client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn data(&self) -> PathBuf {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .data
            .clone()
    }
    fn text(&self, english: &str, spanish: &str) -> String {
        if self
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .language
            .starts_with("es")
        {
            spanish
        } else {
            english
        }
        .to_owned()
    }
    fn has_session(&self) -> bool {
        self.data().join("account.json").exists()
    }
    fn network(&self) -> Result<(), RpcError> {
        if self.online.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(RpcError::new(
                -32005,
                self.text(
                    "Offline mode is enabled.",
                    "El modo sin conexión está activado.",
                ),
            ))
        }
    }

    async fn initialize(&self, params: Value) -> RpcResult {
        if self.initialized.load(Ordering::Acquire) || params["protocol"] != 1 {
            return Err(RpcError::new(
                -32600,
                "Expected one protocol 1 initialization",
            ));
        }
        let data = params["data_dir"]
            .as_str()
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| {
                RpcError::invalid_params("An absolute private data directory is required")
            })?;
        std::fs::create_dir_all(&data)
            .map_err(|_| RpcError::new(-32603, "Cannot create private data directory"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| RpcError::new(-32603, "Cannot protect private storage"))?;
        }
        let language = params["locale"].as_str().unwrap_or("es").to_owned();
        let localized = self.client().with_locale(&language);
        *self.client.lock().unwrap_or_else(|e| e.into_inner()) = localized;
        items::set_lang(language.split(['-', '_', '.']).next().unwrap_or("en"));
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = Config {
            data: data.clone(),
            language: language.clone(),
        };
        self.online
            .store(params["network"]["enabled"] == true, Ordering::Release);
        let mut wizard = self.wizard.lock().await;
        wizard.language = language;
        let saved = data.join("preferences.json");
        if std::fs::metadata(&saved).is_ok_and(|m| m.len() <= 512) {
            if let Ok(raw) = std::fs::read(&saved) {
                if let Ok(value) = serde_json::from_slice::<Value>(&raw) {
                    if let Some(q) = value["quality"].as_str().filter(|q| quality(q).is_some()) {
                        wizard.quality = q.to_owned();
                    }
                }
            }
        }
        let account = data.join("account.json");
        if std::fs::metadata(&account).is_ok_and(|m| m.len() <= 8192) {
            if let Ok(Some(credentials)) = Credentials::load(&data) {
                let signed_in = self.client().with_arl(&credentials.arl);
                *self.client.lock().unwrap_or_else(|e| e.into_inner()) = signed_in;
                wizard.screen = Screen::Session;
            }
        }
        self.initialized.store(true, Ordering::Release);
        Ok(
            json!({"protocol":1,"capabilities":{"search":true,"pages":true,"settings":true,
            "favorites":true,"library":["albums","tracks","artists","playlists"],"scene_search_limit":0,
            "navigation":{"discover":self.text("Home","Inicio"),"library":self.text("My library","Mi biblioteca")}}}),
        )
    }

    async fn authenticated(&self) -> Result<User, RpcError> {
        self.network()?;
        let client = self.client();
        if !client.has_arl() {
            return Err(RpcError::auth_required());
        }
        let user = client.user().await.map_err(RpcError::from)?;
        items::set_user(Some(user.id));
        Ok(user)
    }

    async fn settings_open(&self) -> RpcResult {
        let _guard = self.operations.write().await;
        let mut wizard = self.wizard.lock().await;
        if self.online.load(Ordering::Acquire) && self.has_session() && wizard.account.is_none() {
            self.check_account(&mut wizard, false).await;
        }
        Ok(wizard.document(self.online.load(Ordering::Acquire), self.has_session()))
    }

    async fn check_account(&self, wizard: &mut Wizard, force: bool) {
        let client = self.client();
        let result = if force {
            client.refresh().await
        } else {
            client.user().await
        };
        match result {
            Ok(user) => {
                items::set_user(Some(user.id));
                wizard.account = Some(if user.name.is_empty() {
                    "Deezer".to_owned()
                } else {
                    user.name
                });
                wizard.screen = Screen::Connected;
                wizard.notice = None;
                wizard.changed();
            }
            Err(error) => {
                wizard.account = None;
                wizard.screen = Screen::Session;
                wizard.notice = Some(
                    if error
                        .chain()
                        .any(|e| e.downcast_ref::<SignedOut>().is_some())
                    {
                        self.text(
                            "Your session has expired. Connect a fresh ARL from your browser.",
                            "Tu sesión venció. Conecta una nueva ARL desde el navegador.",
                        )
                    } else {
                        self.text("The account could not be checked. Check your connection and try again.","No se pudo comprobar la cuenta. Revisa la conexión y vuelve a intentarlo.")
                    },
                );
                wizard.changed();
            }
        }
    }

    fn save_quality(&self, value: &str) -> Result<(), RpcError> {
        if quality(value).is_none() {
            return Err(RpcError::invalid_params(
                "Choose an available audio quality",
            ));
        }
        let temporary = self.data().join("preferences.tmp");
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| RpcError::new(-32603, "Cannot save audio preferences"))?;
        file.write_all(json!({"quality":value}).to_string().as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| RpcError::new(-32603, "Cannot save audio preferences"))?;
        std::fs::rename(temporary, self.data().join("preferences.json"))
            .map_err(|_| RpcError::new(-32603, "Cannot save audio preferences"))?;
        Ok(())
    }

    async fn submit(&self, params: Value) -> RpcResult {
        let _guard = self.operations.write().await;
        let mut wizard = self.wizard.lock().await;
        if params["revision"].as_str() != Some(wizard.revision().as_str()) {
            return Err(RpcError::invalid_params(self.text(
                "This form changed. Reopen it.",
                "El formulario cambió. Vuelve a abrirlo.",
            )));
        }
        let values = &params["values"];
        match params["action"].as_str().unwrap_or_default() {
            "continue" => {
                wizard.screen = Screen::Session;
                wizard.notice = None;
                wizard.changed();
            }
            "back" | "cancel" => {
                wizard.screen = Screen::Browser;
                wizard.notice = None;
                wizard.changed();
            }
            "quality" => {
                let selected = values["quality"].as_str().unwrap_or_default();
                self.save_quality(selected)?;
                if wizard.quality != selected {
                    wizard.quality = selected.to_owned();
                    wizard.changed();
                }
            }
            "refresh" => {
                self.network()?;
                self.check_account(&mut wizard, true).await;
            }
            "sign_out" => {
                Credentials::delete(&self.data())
                    .map_err(|_| RpcError::new(-32603, "Cannot remove the saved session"))?;
                let signed_out = self.client().signed_out();
                *self.client.lock().unwrap_or_else(|e| e.into_inner()) = signed_out;
                let mut relay = self.relay.lock().unwrap_or_else(|e| e.into_inner());
                relay.clear();
                *relay = Relay::default();
                self.library.forget(None);
                items::set_user(None);
                wizard.account = None;
                wizard.screen = Screen::Browser;
                wizard.notice = None;
                wizard.changed();
            }
            "sign_in" => {
                self.network()?;
                let session = values["session"].as_str().unwrap_or_default();
                let arl = if session.len() <= 2048 {
                    extract_arl(session)
                } else {
                    None
                };
                if let Some(arl) = arl {
                    let client = self.client().with_arl(&arl);
                    match client.refresh().await {
                        Ok(user) => {
                            Credentials { arl }.save(&self.data()).map_err(|_| {
                                RpcError::new(-32603, "Cannot save the validated session")
                            })?;
                            *self.client.lock().unwrap_or_else(|e| e.into_inner()) = client;
                            {
                                let mut relay =
                                    self.relay.lock().unwrap_or_else(|e| e.into_inner());
                                relay.clear();
                                *relay = Relay::default();
                            }
                            self.library.forget(None);
                            items::set_user(Some(user.id));
                            wizard.account = Some(if user.name.is_empty() {
                                "Deezer".to_owned()
                            } else {
                                user.name
                            });
                            wizard.screen = Screen::Connected;
                            wizard.notice = None;
                            if let Some(q) =
                                values["quality"].as_str().filter(|q| quality(q).is_some())
                            {
                                self.save_quality(q)?;
                                wizard.quality = q.to_owned();
                            }
                        }
                        Err(error) => {
                            wizard.account = None;
                            wizard.screen = Screen::Session;
                            wizard.notice = Some(
                                if error
                                    .chain()
                                    .any(|e| e.downcast_ref::<SignedOut>().is_some())
                                {
                                    self.text("Deezer refused this session. Copy the complete ARL from your signed-in browser and try again.","Deezer rechazó esta sesión. Copia la ARL completa del navegador donde iniciaste sesión y vuelve a intentarlo.")
                                } else {
                                    self.text("The connection failed. Your new session was not saved. Try again when online.","Falló la conexión. No se guardó la nueva sesión. Vuelve a intentarlo cuando haya conexión.")
                                },
                            );
                        }
                    }
                } else {
                    wizard.screen = Screen::Session;
                    wizard.notice=Some(self.text("Paste the complete ARL cookie value, not a web address or password.","Pega el valor completo de la cookie ARL, no una dirección web ni tu contraseña."));
                }
                wizard.changed();
            }
            _ => return Err(RpcError::invalid_params("Unknown account action")),
        }
        Ok(wizard.document(self.online.load(Ordering::Acquire), self.has_session()))
    }

    async fn resolve(&self, params: Value) -> RpcResult {
        let original = catalog::reference(&params["ref"], &["track"])?;
        self.authenticated().await?;
        let output = serde_json::from_value::<resolve::Output>(
            params.get("output").cloned().unwrap_or_else(|| json!({})),
        )
        .map_err(|_| RpcError::invalid_params("Invalid output limits"))?;
        let maximum = quality(&self.wizard.lock().await.quality).unwrap_or(Quality::Flac);
        let relay = self.relay.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let (result, _) =
            resolve::resolve(&self.client(), &relay, &original, &output, maximum).await?;
        let mut value = json!({"url":result["url"],"format":{"codec":result["format"]["codec"]}});
        if let Some(gain) = result["replaygain"]["track_gain"]
            .as_f64()
            .filter(|gain| gain.is_finite() && (-40.0..=40.0).contains(gain))
        {
            value["normalization"] = json!({"gain_db":gain});
        }
        Ok(value)
    }

    async fn handle(&self, method: &str, params: Value) -> RpcResult {
        if !params.is_object() {
            return Err(RpcError::invalid_params("Parameters must be an object"));
        }
        if method == "initialize" {
            return self.initialize(params).await;
        }
        if !self.initialized.load(Ordering::Acquire) {
            return Err(RpcError::new(-32600, "Initialize first"));
        }
        if method == "settings.open" {
            return self.settings_open().await;
        }
        if method == "settings.submit" {
            return self.submit(params).await;
        }
        let _guard = self.operations.read().await;
        self.network()?;
        match method {
            "search" => self.search(params).await,
            "page.get" => self.page(params).await,
            "track.get" => Ok(
                json!({"track":self.get_item(&catalog::reference(&params["ref"],&["track"])?).await?}),
            ),
            "album.get" => self.collection(params, false).await,
            "playlist.get" => self.collection(params, true).await,
            "library.list" => self.library_list(params).await,
            "track.resolve" => self.resolve(params).await,
            "favorites.set" | "page.action" => {
                if method == "page.action" && params["action"] != "favorite" {
                    return Err(RpcError::new(-32601, "Unsupported page action"));
                }
                self.authenticated().await?;
                let reference =
                    catalog::reference(&params["ref"], &["track", "album", "artist", "playlist"])?;
                let on = params["on"]
                    .as_bool()
                    .ok_or_else(|| RpcError::invalid_params("Missing favourite state"))?;
                upstream::set_favorite(&self.client(), &self.library, &reference, on).await?;
                Ok(json!({}))
            }
            _ => Err(RpcError::new(-32601, "Method unavailable")),
        }
    }

    fn safe_error(&self, error: RpcError) -> RpcError {
        let message = match error.code {
            -32001 => self.text(
                "Connect your Deezer account in the plugin settings to continue.",
                "Conecta tu cuenta de Deezer en los ajustes del plugin para continuar.",
            ),
            -32002 => self.text(
                "This item is no longer available.",
                "Este elemento ya no está disponible.",
            ),
            -32003 => self.text(
                "This track or format is not available for your account.",
                "Esta pista o formato no está disponible para tu cuenta.",
            ),
            -32004 => self.text(
                "The service limited requests. Wait before retrying.",
                "El servicio limitó las consultas. Espera antes de volver a intentarlo.",
            ),
            -32005 => self.text(
                "Check the connection and offline setting, then try again.",
                "Revisa la conexión y el modo sin conexión; después vuelve a intentarlo.",
            ),
            -32601 => return RpcError::new(-32601, "Method unavailable"),
            -32602 => self.text(
                "Check the form or request and try again.",
                "Revisa el formulario o la solicitud y vuelve a intentarlo.",
            ),
            _ => self.text(
                "The operation could not be completed. Try again.",
                "No se pudo completar la operación. Vuelve a intentarlo.",
            ),
        };
        RpcError::new(error.code, message)
    }
}

fn quality(key: &str) -> Option<Quality> {
    match key {
        "flac" => Some(Quality::Flac),
        "mp3_320" => Some(Quality::Mp3_320),
        "mp3_128" => Some(Quality::Mp3_128),
        _ => None,
    }
}
