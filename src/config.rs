use std::borrow::Cow;

use eyre::OptionExt;
use serde::Deserialize;
use triomphe::Arc;

#[derive(Deserialize)]
pub struct ConfigData<'a> {
    pub username: Cow<'a, str>,
    pub token: Cow<'a, str>,
    pub channels: Vec<Cow<'a, str>>,
}

pub type Config = Arc<ConfigData<'static>>;

pub fn from_config_dir() -> eyre::Result<Config> {
    let dir = dirs::config_dir()
        .ok_or_eyre("configuration file not found")?
        .join("tuige/config.toml");

    Ok(Arc::new(
        std::fs::read_to_string(&dir).map(|s| toml::from_str::<ConfigData>(&s))??,
    ))
}
