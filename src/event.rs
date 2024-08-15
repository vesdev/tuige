use core::panic;

use color_eyre::eyre;
use crossterm::event::EventStream;
use futures::{future::Fuse, stream::Next, FutureExt, StreamExt};
use tokio::{
    select,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};

use crate::{config::Config, request::ReqCache};

#[derive(Clone, PartialEq, PartialOrd)]
pub struct Message {
    pub channel: String,
    pub username: String,
    pub msg: String,
}

/// Events
pub mod ev {
    use super::Message;

    /// Incoming events
    #[derive(PartialEq, PartialOrd)]
    pub enum In {
        Key(crossterm::event::KeyEvent),
        Message(Message),
        Redraw,
    }

    /// Outgoing events
    #[derive(PartialEq, PartialOrd)]
    pub enum Send {
        Message(Message),
    }
}

pub struct EventHandler {
    handler_rx: UnboundedReceiver<ev::Send>,
    event_tx: UnboundedSender<ev::In>,
    cfg: Config,
    req: ReqCache,
}

impl EventHandler {
    pub fn new(
        cfg: Config,
        disk_cache_dir: String,
        event_tx: UnboundedSender<ev::In>,
        handler_rx: UnboundedReceiver<ev::Send>,
    ) -> Self {
        Self {
            event_tx,
            handler_rx,
            cfg,
            req: ReqCache::new(disk_cache_dir),
        }
    }

    pub async fn run(&mut self) -> eyre::Result<()> {
        let mut reader = crossterm::event::EventStream::new();

        let mut tmi_event_tx = self.event_tx.clone();
        let cfg = self.cfg.clone();

        //TODO: multiple clients per user
        let mut client = tmi::Client::builder()
            .credentials(tmi::Credentials {
                login: self.cfg.username.to_string(),
                token: Some(self.cfg.token.to_string()),
            })
            .connect()
            .await?;

        let (message_tx, message_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            client.join_all(&cfg.channels).await.unwrap();
            let mut message_rx = message_rx;
            loop {
                Self::tmi_event(&cfg, &mut client, &mut tmi_event_tx, &mut message_rx)
                    .await
                    .unwrap();
            }
        });

        loop {
            let term_event = reader.next().fuse();

            select! {
                e = self.handler_rx.recv() => {
                    if let Some(e) = e {
                        match e {
                            ev::Send::Message(message) => {
                                let _ = message_tx.send(message);
                            }
                        }
                        // Placeholder
                    }
                }
                _ = Self::crossterm_event(term_event, &mut self.event_tx) => {}
            }
        }
    }

    async fn crossterm_event(
        term_event: Fuse<Next<'_, EventStream>>,
        event_tx: &mut UnboundedSender<ev::In>,
    ) -> eyre::Result<()> {
        #[allow(clippy::collapsible_match, clippy::single_match)]
        if let Some(Ok(e)) = term_event.await {
            match e {
                crossterm::event::Event::Key(k) => {
                    event_tx.send(ev::In::Key(k))?;
                }
                crossterm::event::Event::Resize(_, _) => {
                    event_tx.send(ev::In::Redraw)?;
                }
                _ => (),
            }
        }
        Ok(())
    }

    async fn tmi_event(
        cfg: &Config,
        client: &mut tmi::Client,
        event_tx: &mut UnboundedSender<ev::In>,
        message_rx: &mut UnboundedReceiver<Message>,
    ) -> eyre::Result<()> {
        select! {
            msg = client.recv() => {
                match msg?.as_typed()? {
                    tmi::Message::Privmsg(msg) => {
                        event_tx.send(ev::In::Message(Message {
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
            }
            msg = message_rx.recv() => {
                if let Some(msg) = msg {
                    client.privmsg(&msg.channel, &msg.msg).send().await?;
                }
            }
        }

        Ok(())
    }
}
