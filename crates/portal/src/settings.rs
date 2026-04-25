//! `org.freedesktop.impl.portal.Settings` implementation.
#![allow(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use zbus::interface;
use zvariant::{OwnedValue, Str, StructureBuilder};

use crate::Config;

/// Handler for the Settings portal interface.
pub struct SettingsPortal {
    config: Arc<RwLock<Config>>,
}

impl SettingsPortal {
    /// Create a new [`SettingsPortal`] backed by the given config.
    pub fn new(config: Arc<RwLock<Config>>) -> Self {
        Self { config }
    }

    fn str_val(s: &str) -> OwnedValue {
        OwnedValue::from(Str::from(s.to_owned()))
    }

    /// Build a value for a single settings key.
    fn value_for_key(config: &Config, namespace: &str, key: &str) -> Option<OwnedValue> {
        match namespace {
            "org.freedesktop.appearance" => match key {
                "color-scheme" => Some(OwnedValue::from(config.color_scheme)),
                "contrast" => Some(OwnedValue::from(config.contrast)),
                "accent-color" => {
                    let [r, g, b] = config.accent_color;
                    let s = StructureBuilder::new()
                        .add_field(r)
                        .add_field(g)
                        .add_field(b)
                        .build()
                        .ok()?;
                    OwnedValue::try_from(s).ok()
                }
                _ => None,
            },
            "org.gnome.desktop.interface" => match key {
                "color-scheme" => {
                    let s = match config.color_scheme {
                        1 => "prefer-dark",
                        2 => "prefer-light",
                        _ => "default",
                    };
                    Some(Self::str_val(s))
                }
                "gtk-theme" => Some(Self::str_val(&config.gtk_theme)),
                "icon-theme" => Some(Self::str_val(&config.icon_theme)),
                "cursor-theme" => Some(Self::str_val(&config.cursor_theme)),
                "cursor-size" => Some(OwnedValue::from(config.cursor_size)),
                "font-name" => Some(Self::str_val(&config.font_name)),
                "text-scaling-factor" => Some(OwnedValue::from(config.text_scaling_factor)),
                _ => None,
            },
            _ => None,
        }
    }

    fn namespace_keys(namespace: &str) -> &'static [&'static str] {
        match namespace {
            "org.freedesktop.appearance" => &["color-scheme", "contrast", "accent-color"],
            "org.gnome.desktop.interface" => &[
                "color-scheme",
                "gtk-theme",
                "icon-theme",
                "cursor-theme",
                "cursor-size",
                "font-name",
                "text-scaling-factor",
            ],
            _ => &[],
        }
    }

    const NAMESPACES: &'static [&'static str] = &[
        "org.freedesktop.appearance",
        "org.gnome.desktop.interface",
    ];
}

#[allow(missing_docs)]
#[interface(name = "org.freedesktop.impl.portal.Settings")]
impl SettingsPortal {
    /// Return all settings matching the provided namespace patterns.
    ///
    /// Empty `namespaces` means return everything.  Supports `*` suffix glob.
    async fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> zbus::fdo::Result<HashMap<String, HashMap<String, OwnedValue>>> {
        let config = self.config.read().await;

        let matches = |ns: &str| -> bool {
            if namespaces.is_empty() {
                return true;
            }
            namespaces.iter().any(|n| {
                if let Some(prefix) = n.strip_suffix('*') {
                    ns.starts_with(prefix)
                } else {
                    n == ns
                }
            })
        };

        let mut result: HashMap<String, HashMap<String, OwnedValue>> = HashMap::new();
        for &ns in Self::NAMESPACES {
            if !matches(ns) {
                continue;
            }
            let mut ns_map = HashMap::new();
            for &key in Self::namespace_keys(ns) {
                if let Some(v) = Self::value_for_key(&config, ns, key) {
                    ns_map.insert(key.to_owned(), v);
                }
            }
            result.insert(ns.to_owned(), ns_map);
        }

        Ok(result)
    }

    /// Return a single settings value.
    async fn read(
        &self,
        namespace: &str,
        key: &str,
    ) -> zbus::fdo::Result<OwnedValue> {
        let config = self.config.read().await;
        Self::value_for_key(&config, namespace, key).ok_or_else(|| {
            zbus::fdo::Error::Failed(format!("unknown setting {namespace}/{key}"))
        })
    }

    /// Signal emitted when a setting changes.
    #[zbus(signal)]
    async fn setting_changed(
        ctx: &zbus::object_server::SignalEmitter<'_>,
        namespace: &str,
        key: &str,
        value: &zvariant::Value<'_>,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_portal() -> SettingsPortal {
        SettingsPortal::new(Arc::new(RwLock::new(Config::default())))
    }

    #[test]
    fn value_for_color_scheme() {
        let cfg = Config::default();
        let v = SettingsPortal::value_for_key(&cfg, "org.freedesktop.appearance", "color-scheme");
        assert!(v.is_some());
    }

    #[test]
    fn value_for_gnome_color_scheme_dark() {
        let cfg = Config { color_scheme: 1, ..Config::default() };
        let v = SettingsPortal::value_for_key(&cfg, "org.gnome.desktop.interface", "color-scheme");
        assert!(v.is_some());
    }

    #[test]
    fn value_for_accent_color() {
        let cfg = Config::default();
        let v = SettingsPortal::value_for_key(&cfg, "org.freedesktop.appearance", "accent-color");
        assert!(v.is_some());
    }

    #[test]
    fn value_for_unknown_key_is_none() {
        let cfg = Config::default();
        let v = SettingsPortal::value_for_key(&cfg, "org.freedesktop.appearance", "nonexistent");
        assert!(v.is_none());
    }

    #[test]
    fn value_for_unknown_namespace_is_none() {
        let cfg = Config::default();
        let v = SettingsPortal::value_for_key(&cfg, "com.example.unknown", "key");
        assert!(v.is_none());
    }

    #[test]
    fn portal_new_does_not_panic() {
        let _ = make_portal();
    }
}
