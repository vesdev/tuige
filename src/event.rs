use crossterm::event::EventStream;
use futures::{future::Fuse, stream::Next, FutureExt, StreamExt};
use tokio::{
    select,
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
};

use crate::config::Config;

#[derive(PartialEq, PartialOrd)]
pub enum Event {
    Key(crossterm::event::KeyEvent),
    Message(Message),
    Redraw,
}

#[derive(PartialEq, PartialOrd)]
pub enum HandlerEvent {}

#[derive(Clone, PartialEq, PartialOrd)]
pub struct Message {
    pub channel: String,
    pub username: String,
    pub msg: String,
}

pub struct EventHandler {
    handler_rx: UnboundedReceiver<HandlerEvent>,
    event_tx: UnboundedSender<Event>,
    cfg: Config,
}

impl EventHandler {
    pub fn new(
        cfg: Config,
        event_tx: UnboundedSender<Event>,
        handler_rx: UnboundedReceiver<HandlerEvent>,
    ) -> Self {
        Self {
            event_tx,
            handler_rx,
            cfg,
        }
    }

    pub async fn run(&mut self) -> eyre::Result<()> {
        let mut reader = crossterm::event::EventStream::new();
        let mut client = tmi::Client::builder()
            .credentials(tmi::Credentials {
                login: self.cfg.username.to_string(),
                token: Some(self.cfg.token.to_string()),
            })
            .connect()
            .await?;

        let mut tmi_event_tx = self.event_tx.clone();
        let cfg = self.cfg.clone();

        tokio::spawn(async move {
            client.join_all(&cfg.channels).await.unwrap();
            loop {
                Self::tmi_event(&cfg, &mut client, &mut tmi_event_tx)
                    .await
                    .unwrap();
            }
        });

        loop {
            let term_event = reader.next().fuse();

            select! {
                e = self.handler_rx.recv() => {
                    if let Some(_e) = e {
                        // Placeholder
                    }
                }
                _ = Self::crossterm_event(term_event, &mut self.event_tx) => {}
                // _ = Self::tmi_event(&self.cfg, &mut client, &mut tmi_event_tx) => {}
            }
        }
    }

    async fn crossterm_event(
        term_event: Fuse<Next<'_, EventStream>>,
        event_tx: &mut UnboundedSender<Event>,
    ) -> eyre::Result<()> {
        #[allow(clippy::collapsible_match, clippy::single_match)]
        if let Some(Ok(e)) = term_event.await {
            match e {
                crossterm::event::Event::Key(k) => {
                    event_tx.send(Event::Key(k))?;
                }
                crossterm::event::Event::Resize(_, _) => {
                    event_tx.send(Event::Redraw)?;
                }
                _ => (),
            }
        }
        Ok(())
    }

    async fn tmi_event<'a>(
        cfg: &Config,
        client: &mut tmi::Client,
        event_tx: &mut UnboundedSender<Event>,
    ) -> eyre::Result<()> {
        let msg = client.recv().await?;
        match msg.as_typed()? {
            tmi::Message::Privmsg(msg) => {
                event_tx.send(Event::Message(Message {
                    channel: msg.channel().into(),
                    username: msg.sender().name().into(),
                    msg: msg.text().into(),
                }))?;
            }
            tmi::Message::Reconnect => {
                client.reconnect().await?;
                client.join_all(&cfg.channels).await?;
            }
            tmi::Message::Ping(ping) => {
                client.pong(&ping).await?;
            }
            _ => {}
        }
        Ok(())
    }
}
