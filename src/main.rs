use tui::Tui;

mod config;
mod tui;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let mut tui = Tui::new()?;
    tui.run().await
}
