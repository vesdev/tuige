use std::{collections::VecDeque, io::Stdout, time::Duration};

use crossterm::{
    event::{EventStream, KeyCode, KeyEvent},
    terminal,
};
use futures::{
    future::Fuse,
    stream::{Chunks, Next},
    FutureExt, StreamExt,
};
use indexmap::IndexMap;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListDirection},
    Terminal,
};
use tokio::{
    select,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tui_textarea::TextArea;

pub struct Chat {
    messages: VecDeque<Message>,
}

impl Chat {
    pub fn new() -> Self {
        Self {
            messages: VecDeque::with_capacity(100),
        }
    }

    pub fn push_message(&mut self, message: Message) {
        if self.messages.len() == self.messages.capacity() {
            self.messages.pop_back();
            self.messages.push_front(message);
        } else {
            self.messages.push_front(message);
        }
    }

    pub fn list(&mut self) -> List {
        let list = self.messages.iter().cloned().enumerate().map(|(i, m)| {
            Line::from(vec![
                Span::styled(m.username, Style::default().blue()),
                Span::styled(": ", Style::default()),
                Span::styled(m.msg, Style::default()),
            ])
            .bg(if i % 2 == 0 {
                Color::Black
            } else {
                Color::Reset
            })
        });
        List::new(list).direction(ListDirection::BottomToTop).block(
            Block::bordered()
                .title("#forsen")
                .title_alignment(Alignment::Center),
        )
    }
}

pub enum PopupKind {
    JoinChannel,
}

pub struct Popup {
    kind: PopupKind,
}

impl Popup {
    pub fn new(kind: PopupKind) -> Self {
        Self { kind }
    }
}

pub struct Tui<'a> {
    term: Terminal<CrosstermBackend<Stdout>>,
    task: JoinHandle<()>,
    event_rx: UnboundedReceiver<Event>,
    handler_tx: UnboundedSender<HandlerEvent>,
    tabs: IndexMap<String, Chat>,
    active_tab: Option<String>,
    popup: Option<Popup>,
    textarea: TextArea<'a>,
}

impl Tui<'_> {
    pub fn new() -> eyre::Result<Self> {
        let backend = CrosstermBackend::new(std::io::stdout());
        let term = Terminal::new(backend)?;

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (handler_tx, handler_rx) = mpsc::unbounded_channel();
        let mut handler = EventHandler::new(event_tx, handler_rx, Duration::from_millis(60));
        let task = tokio::spawn(async move {
            handler.run().await.unwrap();
        });

        let mut textarea = TextArea::default();
        textarea.set_block(Block::default().borders(Borders::ALL));

        Ok(Self {
            term,
            task,
            event_rx,
            handler_tx,
            tabs: IndexMap::from([
                ("#forsen".into(), Chat::new()),
                ("#zoil".into(), Chat::new()),
            ]),
            active_tab: Some("#forsen".into()),
            popup: None,
            textarea,
        })
    }

    pub async fn run(&mut self) -> eyre::Result<()> {
        self.enter()?;

        loop {
            let Some(e) = self.event_rx.recv().await else {
                continue;
            };
            match e {
                Event::KeyEvent(k) => match k {
                    KeyEvent {
                        code: KeyCode::Char('q'),
                        ..
                    } => {
                        break;
                    }
                    _ => {
                        // WeirdChamp forced to use crossterm keyevent
                        self.textarea.input(k);
                    }
                },
                Event::Draw => {
                    self.draw().await?;
                }
                Event::Message(message) => {
                    if let Some(c) = self.tabs.get_mut(&message.channel) {
                        c.push_message(message);
                    }
                }
            }
        }

        self.leave()
    }

    fn enter(&self) -> eyre::Result<()> {
        terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
        Ok(())
    }

    fn leave(&self) -> eyre::Result<()> {
        crossterm::execute!(std::io::stdout(), terminal::LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;
        Ok(())
    }

    async fn draw(&mut self) -> eyre::Result<()> {
        self.term.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(95), Constraint::Percentage(5)])
                .split(frame.area());

            frame.render_widget(&self.textarea, chunks[1]);
            let active = self.active_tab.clone().unwrap_or("".into());

            let mut tabs = Block::bordered().title_alignment(Alignment::Center);
            for (name, _) in &mut self.tabs {
                tabs = tabs.title(if name == &active {
                    name.clone().into()
                } else {
                    name.clone().dim()
                });
            }

            if let Some(active) = self.tabs.get_mut(&active) {
                frame.render_widget(active.list().block(tabs), chunks[0]);
            } else {
                frame.render_widget(tabs, chunks[0]);
            }
        })?;
        Ok(())
    }
}

#[derive(PartialEq, PartialOrd)]
pub enum Event {
    Draw,
    KeyEvent(crossterm::event::KeyEvent),
    Message(Message),
}

#[derive(PartialEq, PartialOrd)]
pub enum HandlerEvent {
    Join(String),
}

#[derive(Clone, PartialEq, PartialOrd)]
pub struct Message {
    channel: String,
    username: String,
    msg: String,
}

pub struct EventHandler {
    handler_rx: UnboundedReceiver<HandlerEvent>,
    event_tx: UnboundedSender<Event>,
    frame_duration: Duration,
    tmi_channel: String,
}

impl EventHandler {
    pub fn new(
        event_tx: UnboundedSender<Event>,
        handler_rx: UnboundedReceiver<HandlerEvent>,
        frame_duration: Duration,
    ) -> Self {
        Self {
            event_tx,
            handler_rx,
            frame_duration,
            tmi_channel: "#forsen".into(),
        }
    }

    pub async fn run(&mut self) -> eyre::Result<()> {
        let mut draw_interval = tokio::time::interval(self.frame_duration);
        let mut reader = crossterm::event::EventStream::new();
        let mut client = tmi::Client::anonymous().await?;
        let mut channels = vec![self.tmi_channel.clone()];
        client.join_all(&channels).await?;

        let mut tmi_event_tx = self.event_tx.clone();
        loop {
            let term_event = reader.next().fuse();
            let draw_delay = draw_interval.tick();

            select! {
                e = self.handler_rx.recv() => {
                    if let Some(e) = e {
                        match e {
                            HandlerEvent::Join(c) => {
                                channels = vec![c];
                            },
                        }
                    }
                }
                _ = draw_delay => {
                    self.event_tx.send(Event::Draw)?;
                }
                _ = Self::crossterm_event(term_event, &mut self.event_tx) => {}
                _ = Self::tmi_event(&mut client, &channels, &mut tmi_event_tx) => {}
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
                    event_tx.send(Event::KeyEvent(k))?;
                }
                _ => (),
            }
        }
        Ok(())
    }

    async fn tmi_event(
        client: &mut tmi::Client,
        channels: &Vec<String>,
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
                client.join_all(channels).await?;
            }
            tmi::Message::Ping(ping) => {
                client.pong(&ping).await?;
            }
            _ => {}
        }
        Ok(())
    }
}
