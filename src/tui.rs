use std::{collections::VecDeque, io::Stdout, time::Duration};

use crossterm::{
    event::{EventStream, KeyCode, KeyEvent},
    terminal,
};
use futures::{future::Fuse, stream::Next, FutureExt, StreamExt};
use indexmap::IndexMap;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListDirection},
    Frame, Terminal,
};
use tokio::{
    select,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};
use tui_textarea::TextArea;

use crate::config::Config;

pub struct Chat<'a> {
    lines: VecDeque<Line<'a>>,
    bg_darken: bool,
}

impl<'a> Chat<'a> {
    pub fn new() -> Self {
        Self {
            lines: VecDeque::with_capacity(100),
            bg_darken: false,
        }
    }

    pub fn push_message(&mut self, mention_finder: &memchr::memmem::Finder, message: Message) {
        let found_mention = mention_finder.find(message.msg.as_bytes()).is_some();
        let line = Line::from(vec![
            Span::styled(message.username, Style::default().blue()),
            Span::styled(": ", Style::default()),
            Span::styled(message.msg, Style::default()),
        ])
        .bg({
            if found_mention {
                Color::Red
            } else if self.bg_darken {
                Color::Black
            } else {
                Color::Reset
            }
        });

        self.bg_darken = !self.bg_darken;

        if self.lines.len() == self.lines.capacity() {
            self.lines.pop_back();
            self.lines.push_front(line);
        } else {
            self.lines.push_front(line);
        }
    }

    //TODO: remove clones
    pub fn list(&self, title: String) -> List<'a> {
        List::new(self.lines.clone())
            .direction(ListDirection::BottomToTop)
            .block(
                Block::bordered()
                    .title(title)
                    .title_alignment(Alignment::Center),
            )
    }
}

#[allow(unused)]
pub struct Tui<'a> {
    tabs: IndexMap<String, Chat<'a>>,
    active_tab: Option<String>,
    textarea: TextArea<'a>,
}

impl Default for Tui<'_> {
    fn default() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(Block::default().borders(Borders::ALL));

        Self {
            active_tab: None,
            tabs: IndexMap::default(),
            textarea,
        }
    }
}

impl Tui<'_> {
    pub async fn run(&mut self, cfg: Config) -> eyre::Result<()> {
        let mut term = self.enter()?;
        self.active_tab = Some(cfg.channels.first().map_or("".into(), |s| s.to_string()));
        self.tabs = IndexMap::from_iter(
            cfg.channels
                .iter()
                .map(|c| (c.clone().into_owned(), Chat::new())),
        );

        // Draw first frame early as possible
        term.draw(|frame| {
            self.render(frame);
        })?;

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (_handler_tx, handler_rx) = mpsc::unbounded_channel();
        let mut handler =
            EventHandler::new(cfg.clone(), event_tx, handler_rx, Duration::from_millis(60));

        tokio::spawn(async move {
            handler.run().await.unwrap();
        });

        let mention_finder = memchr::memmem::Finder::new(cfg.username.as_ref());
        loop {
            let Some(e) = event_rx.recv().await else {
                continue;
            };
            match e {
                Event::Key(k) => match k {
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
                    term.draw(|frame| {
                        self.render(frame);
                    })?;
                }
                Event::Message(message) => {
                    if let Some(c) = self.tabs.get_mut(&message.channel) {
                        c.push_message(&mention_finder, message);
                    }
                }
            }
        }

        self.leave(term)
    }

    fn enter(&self) -> eyre::Result<Terminal<CrosstermBackend<Stdout>>> {
        let backend = CrosstermBackend::new(std::io::stdout());
        let term = Terminal::new(backend)?;
        terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
        Ok(term)
    }

    fn leave(&self, _term: Terminal<CrosstermBackend<Stdout>>) -> eyre::Result<()> {
        crossterm::execute!(std::io::stdout(), terminal::LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;
        Ok(())
    }

    fn render(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(90), Constraint::Percentage(10)])
            .split(frame.area());

        frame.render_widget(&self.textarea, chunks[1]);
        let active = self.active_tab.clone().unwrap_or("".into());

        let mut tabs = Block::bordered().title_alignment(Alignment::Center);
        for (name, _) in &self.tabs {
            tabs = tabs.title(if name == &active {
                name.clone().into()
            } else {
                name.clone().dim()
            });
        }

        if let Some(active_chat) = self.tabs.get(&active) {
            frame.render_widget(active_chat.list(active).block(tabs), chunks[0]);
        } else {
            frame.render_widget(tabs, chunks[0]);
        }
    }
}

#[derive(PartialEq, PartialOrd)]
pub enum Event {
    Draw,
    Key(crossterm::event::KeyEvent),
    Message(Message),
}

#[derive(PartialEq, PartialOrd)]
pub enum HandlerEvent {}

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
    cfg: Config,
}

impl EventHandler {
    pub fn new(
        cfg: Config,
        event_tx: UnboundedSender<Event>,
        handler_rx: UnboundedReceiver<HandlerEvent>,
        frame_duration: Duration,
    ) -> Self {
        Self {
            event_tx,
            handler_rx,
            frame_duration,
            cfg,
        }
    }

    pub async fn run(&mut self) -> eyre::Result<()> {
        let mut draw_interval = tokio::time::interval(self.frame_duration);
        let mut reader = crossterm::event::EventStream::new();
        let mut client = tmi::Client::builder()
            .credentials(tmi::Credentials {
                login: self.cfg.username.to_string(),
                token: Some(self.cfg.token.to_string()),
            })
            .connect()
            .await?;

        client.join_all(&self.cfg.channels).await?;

        let mut tmi_event_tx = self.event_tx.clone();
        loop {
            let term_event = reader.next().fuse();
            let draw_delay = draw_interval.tick();

            select! {
                e = self.handler_rx.recv() => {
                    if let Some(_e) = e {
                        // Placeholder
                    }
                }
                _ = draw_delay => {
                    self.event_tx.send(Event::Draw)?;
                }
                _ = Self::crossterm_event(term_event, &mut self.event_tx) => {}
                _ = Self::tmi_event(&self.cfg, &mut client, &mut tmi_event_tx) => {}
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
