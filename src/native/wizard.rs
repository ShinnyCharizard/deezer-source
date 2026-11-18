//! Stateless account form rendering. Credentials never enter this view model.

use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Screen {
    Browser,
    Session,
    Connected,
}

pub struct Wizard {
    pub screen: Screen,
    pub revision: u64,
    pub language: String,
    pub account: Option<String>,
    /// A localized, sanitized message supplied by the account controller.
    pub notice: Option<String>,
    pub quality: String,
}

impl Default for Wizard {
    fn default() -> Self {
        Self {
            screen: Screen::Browser,
            revision: 1,
            language: "es".into(),
            account: None,
            notice: None,
            quality: "flac".into(),
        }
    }
}

impl Wizard {
    pub fn revision(&self) -> String {
        self.revision.to_string()
    }

    /// Call after a real visible state change, never merely after rendering.
    pub fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn text<'a>(&self, english: &'a str, spanish: &'a str) -> &'a str {
        if self
            .language
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .eq_ignore_ascii_case("es")
        {
            spanish
        } else {
            english
        }
    }

    fn action(&self, key: &str, english: &str, spanish: &str, fields: &[&str]) -> Value {
        let mut action = json!({"key": key, "label": self.text(english, spanish), "fields": fields});
        if key == "sign_out" { action["destructive"] = json!("account"); }
        action
    }

    fn quality_field(&self) -> Value {
        json!({
            "key": "quality",
            "label": self.text("Maximum audio quality", "Calidad máxima de audio"),
            "control": {"type": "select", "choices": [
                {"value": "flac", "label": self.text("FLAC · CD quality", "FLAC · calidad CD")},
                {"value": "mp3_320", "label": "MP3 · 320 kbps"},
                {"value": "mp3_128", "label": "MP3 · 128 kbps"}
            ]}
        })
    }

    /// `online` is the network permission, not proof of account validity.
    pub fn document(&self, online: bool, has_saved_session: bool) -> Value {
        // A remembered screen must never claim an account is connected after
        // the credential or validated account summary has been cleared.
        let screen = match self.screen {
            Screen::Connected if !has_saved_session || self.account.is_none() => Screen::Session,
            screen => screen,
        };
        let mut fields = Vec::new();
        let mut actions = Vec::new();
        let mut content = Vec::new();
        let mut links = Vec::new();
        let mut values = json!({});
        let mut notices = Vec::new();
        if !online {
            notices.push(self.text(
                "Network access is disabled. Enable it in the plugin permissions to check your account or play music.",
                "El acceso a la red está desactivado. Actívalo en los permisos del plugin para comprobar tu cuenta o reproducir música.",
            ));
        }
        if let Some(notice) = self.notice.as_deref().filter(|notice| !notice.is_empty()) {
            notices.push(notice);
        }

        let (id, step, label) = match screen {
            Screen::Browser => {
                content.push(json!({"text": self.text(
                    "Sign in to Deezer in your usual browser. Already signed in? Choose Continue.",
                    "Inicia sesión en Deezer desde tu navegador habitual. ¿Ya entraste? Pulsa Continuar.",
                )}));
                content.push(json!({"text": self.text(
                    "Browser sign-in does not connect this plugin automatically. The next step explains how to copy your own session cookie (ARL). Your password stays in the browser.",
                    "Entrar en el navegador no conecta el plugin automáticamente. El siguiente paso explica cómo copiar la cookie de tu propia sesión (ARL). Tu contraseña se queda en el navegador.",
                )}));
                links.push(json!({
                    "label": self.text("Open Deezer", "Abrir Deezer"),
                    "url": "https://www.deezer.com/login", "primary": true
                }));
                actions.push(self.action("continue", "Continue", "Continuar", &[]));
                if has_saved_session {
                    content.push(json!({"text": self.text(
                        "A session is saved on this device. Check it to reconnect, or continue to replace it.",
                        "Hay una sesión guardada en este dispositivo. Compruébala para reconectar o continúa para sustituirla.",
                    )}));
                    actions.push(self.action(
                        "refresh",
                        "Check saved session",
                        "Comprobar sesión guardada",
                        &[],
                    ));
                }
                actions.push(self.action("cancel", "Cancel setup", "Cancelar configuración", &[]));
                (
                    "browser",
                    1,
                    self.text(
                        "Step 1 of 3 · Open your account",
                        "Paso 1 de 3 · Abre tu cuenta",
                    ),
                )
            }
            Screen::Session => {
                content.push(json!({"text": self.text(
                    "On the signed-in Deezer tab, open the browser's developer tools. Under Cookies, select the Deezer domain, find arl and copy only its Value.",
                    "En la pestaña de Deezer con tu sesión abierta, abre las herramientas de desarrollo. En Cookies, selecciona el dominio de Deezer, busca arl y copia solo su Valor.",
                )}));
                content.push(json!({"text": self.text(
                    "Firefox: F12 → Storage → Cookies. Chrome / Edge: F12 → Application → Storage → Cookies. On macOS, use the browser menu to open developer tools.",
                    "Firefox: F12 → Almacenamiento → Cookies. Chrome / Edge: F12 → Aplicación → Almacenamiento → Cookies. En macOS, abre las herramientas desde el menú del navegador.",
                )}));
                content.push(json!({"text": self.text(
                    "Safari: enable developer features in Settings → Advanced, then Develop → Show Web Inspector → Storage → Cookies. If arl is missing, sign in again and reload the Deezer tab.",
                    "Safari: activa las funciones para desarrolladores en Ajustes → Avanzado; luego Desarrollo → Mostrar inspector web → Almacenamiento → Cookies. Si no aparece arl, vuelve a iniciar sesión y recarga Deezer.",
                )}));
                content.push(json!({"text": self.text(
                    "Paste it below, then choose Connect. ARL grants access to your account: keep it private. It is saved in this plugin's local data and is never shown here again.",
                    "Pégala abajo y pulsa Conectar. ARL da acceso a tu cuenta: mantenla privada. Se guarda en los datos locales del plugin y no vuelve a mostrarse aquí.",
                )}));
                fields.push(json!({
                    "key": "session", "label": self.text("Your session cookie (ARL)", "Cookie de tu sesión (ARL)"),
                    "required": true, "control": {"type": "secret", "max_length": 1024}
                }));
                actions.push(self.action(
                    "sign_in",
                    "Connect",
                    "Conectar",
                    &["session", "quality"],
                ));
                actions.push(self.action("back", "Back", "Atrás", &[]));
                actions.push(self.action("cancel", "Cancel setup", "Cancelar configuración", &[]));
                if has_saved_session {
                    content.push(json!({"text": self.text(
                        "If your saved session expired, sign in again and paste the new ARL. If the check only failed temporarily, retry the saved session.",
                        "Si la sesión guardada caducó, vuelve a iniciar sesión y pega la nueva ARL. Si la comprobación falló temporalmente, reintenta con la sesión guardada.",
                    )}));
                    actions.push(self.action(
                        "refresh",
                        "Retry saved session",
                        "Reintentar sesión guardada",
                        &[],
                    ));
                    actions.push(self.action(
                        "sign_out",
                        "Forget saved session",
                        "Eliminar sesión guardada",
                        &[],
                    ));
                }
                (
                    "session",
                    2,
                    self.text(
                        "Step 2 of 3 · Connect your session",
                        "Paso 2 de 3 · Conecta tu sesión",
                    ),
                )
            }
            Screen::Connected => {
                let account = self.account.as_deref().unwrap_or_default();
                content.push(
                    json!({"text": format!("{} {}", self.text("Account:", "Cuenta:"), account)}),
                );
                content.push(json!({"text": self.text(
                    "Your account is connected. You can browse your library and play music. FLAC availability depends on your subscription and the track; a lower quality may be used when needed.",
                    "Tu cuenta está conectada. Puedes explorar tu biblioteca y reproducir música. La disponibilidad de FLAC depende de tu suscripción y de la pista; se puede usar una calidad menor cuando haga falta.",
                )}));
                content.push(json!({"text": self.text(
                    "Sign out removes the saved session from this plugin. It does not sign you out of your browser.",
                    "Cerrar sesión elimina la sesión guardada del plugin. No cierra la sesión del navegador.",
                )}));
                actions.push(self.action(
                    "quality",
                    "Save quality",
                    "Guardar calidad",
                    &["quality"],
                ));
                actions.push(self.action("refresh", "Check account", "Comprobar cuenta", &[]));
                actions.push(self.action("sign_out", "Sign out", "Cerrar sesión", &[]));
                (
                    "connected",
                    3,
                    self.text(
                        "Step 3 of 3 · Deezer connected",
                        "Paso 3 de 3 · Deezer conectado",
                    ),
                )
            }
        };
        if matches!(screen, Screen::Session | Screen::Connected) {
            fields.push(self.quality_field());
            values["quality"] = json!(match self.quality.as_str() {
                "mp3_128" => "mp3_128",
                "mp3_320" => "mp3_320",
                _ => "flac",
            });
        }
        json!({
            "revision": self.revision(),
            "wizard": {"id": id, "step": step, "total": 3, "waiting": false},
            "progress": {"done": if screen == Screen::Connected { 3 } else { step - 1 }, "total": 3},
            "status": {"phase": if screen == Screen::Connected && online { "passed" } else { "ready" }, "label": label},
            "content": content, "notices": notices, "links": links,
            "form": {"fields": fields, "actions": actions, "unsupported": []},
            "values": values
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action_keys(document: &Value) -> Vec<&str> {
        document["form"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["key"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn browser_requires_manual_continue_without_callback_or_polling() {
        let wizard = Wizard {
            language: "en".into(),
            ..Wizard::default()
        };
        let document = wizard.document(true, false);
        assert_eq!(document["wizard"]["step"], 1);
        assert_eq!(document["wizard"]["waiting"], false);
        assert!(action_keys(&document).contains(&"continue"));
        assert!(document["links"][0].get("capture").is_none());
        let copy = document["content"].to_string();
        assert!(copy.contains("Already signed in?"));
        assert!(copy.contains("does not connect this plugin automatically"));
    }

    #[test]
    fn session_is_secret_and_never_prefilled_on_any_screen() {
        let mut wizard = Wizard {
            account: Some("Test account".into()),
            ..Wizard::default()
        };
        for screen in [Screen::Browser, Screen::Session, Screen::Connected] {
            wizard.screen = screen;
            let document = wizard.document(true, true);
            assert!(document["values"].get("session").is_none());
            assert!(document["values"].get("arl").is_none());
            assert_eq!(document["wizard"]["waiting"], false);
            if screen == Screen::Session {
                assert_eq!(document["form"]["fields"][0]["control"]["type"], "secret");
                assert_eq!(
                    document["form"]["actions"][0]["fields"],
                    json!(["session", "quality"])
                );
            }
        }
    }

    #[test]
    fn expired_or_unverified_session_keeps_recovery_controls() {
        let wizard = Wizard {
            screen: Screen::Connected,
            account: None,
            notice: Some("La sesión caducó. Vuelve a iniciar sesión.".into()),
            ..Wizard::default()
        };
        let document = wizard.document(true, true);
        assert_eq!(document["wizard"]["id"], "session");
        assert_eq!(document["status"]["phase"], "ready");
        for key in ["sign_in", "back", "cancel", "refresh", "sign_out"] {
            assert!(
                action_keys(&document).contains(&key),
                "Missing recovery action: {key}"
            );
        }
        assert_eq!(document["notices"][0], wizard.notice.as_deref().unwrap());
        assert!(document["values"].get("session").is_none());
    }

    #[test]
    fn connected_account_can_change_quality_check_and_sign_out() {
        let wizard = Wizard {
            screen: Screen::Connected,
            account: Some("Test account".into()),
            quality: "mp3_320".into(),
            ..Wizard::default()
        };
        let document = wizard.document(true, true);
        assert_eq!(document["progress"]["done"], 3);
        assert_eq!(document["values"]["quality"], "mp3_320");
        assert_eq!(action_keys(&document), ["quality", "refresh", "sign_out"]);
        assert!(document["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Test account"));
        assert_ne!(wizard.document(true, false)["wizard"]["id"], "connected");
    }

    #[test]
    fn offline_notice_and_revisions_are_stable_until_a_change() {
        let mut wizard = Wizard::default();
        let first = wizard.document(false, false);
        assert_eq!(first, wizard.document(false, false));
        assert!(first["revision"].is_string());
        assert_eq!(first["notices"].as_array().unwrap().len(), 1);
        wizard.screen = Screen::Session;
        wizard.changed();
        assert_ne!(first["revision"], wizard.document(false, false)["revision"]);
    }

    #[test]
    fn regional_spanish_locales_and_english_fallback_are_supported() {
        let mut wizard = Wizard::default();
        for locale in ["es", "es-MX", "es_ES", "ES-mx"] {
            wizard.language = locale.into();
            assert!(wizard.document(true, false)["status"]["label"]
                .as_str()
                .unwrap()
                .starts_with("Paso"));
        }
        wizard.language = "fr".into();
        assert!(wizard.document(true, false)["status"]["label"]
            .as_str()
            .unwrap()
            .starts_with("Step"));
    }
}
