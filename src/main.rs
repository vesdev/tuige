use tui::Tui;

mod config;
mod tui;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cfg = config::from_config_dir()?;
    Tui::default().run(cfg).await
}
