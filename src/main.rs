use color_eyre::eyre;
use eyre::OptionExt;
use tui::Tui;

mod config;
mod event;
mod request;
mod tui;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cfg = config::from_config_dir()?;
    let cache_dir = dirs::cache_dir()
        .ok_or_eyre("unable to find cache directory")?
        .join("tuige");

    Tui.run(cfg, cache_dir.to_str().unwrap().into()).await
}
