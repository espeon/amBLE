use serde::{Deserialize, Serialize};
use std::path::Path;
use std::path::PathBuf;

fn default_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("AMBLE_CONFIG") {
        return PathBuf::from(p);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("amble").join("lights.json");
    }
    if let Ok(cwd) = std::env::current_dir() {
        return cwd.join("lights.json");
    }
    PathBuf::from("lights.json")
}

pub fn config_path() -> PathBuf {
    default_config_path()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightConfig {
    pub key: String,
    pub name: String,
    pub mac: String,
    pub address: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpConfig {
    #[serde(default = "default_http_port")]
    pub port: u16,
    #[serde(default = "default_http_host")]
    pub host: String,
    pub api_key: Option<String>,
}

fn default_http_port() -> u16 {
    2708
}
fn default_http_host() -> String {
    "0.0.0.0".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttConfig {
    pub broker: String,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default = "default_discovery_prefix")]
    pub discovery_prefix: String,
    #[serde(default = "default_topic_prefix")]
    pub topic_prefix: String,
}

fn default_discovery_prefix() -> String {
    "homeassistant".into()
}
fn default_topic_prefix() -> String {
    "amaran".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "netKey")]
    pub net_key: String,
    #[serde(rename = "appKey")]
    pub app_key: String,
    #[serde(rename = "relayHub")]
    pub relay_hub: String,
    pub lights: Vec<LightConfig>,
    pub http: Option<HttpConfig>,
    pub mqtt: Option<MqttConfig>,
}

pub fn load_config(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        anyhow::bail!(
            "lights.json not found at {}.\n\
             Run setup first:  cargo run -- setup\n\
             Or set AMBLE_CONFIG to point to your config file\n\
             (default: ~/.config/amble/lights.json)",
            path.display()
        );
    }
    let contents = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&contents)?)
}

pub fn save_config(config: &Config) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(config)? + "\n")?;
    Ok(())
}
