use std::{collections::VecDeque, io::Stdout};

use color_eyre::config::HookBuilder;
use color_eyre::eyre;
use crossterm::{
    event::{KeyCode, KeyEvent},
    terminal,
};
use indexmap::IndexMap;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListDirection},
    Frame, Terminal,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::{select, sync::mpsc};
use tui_textarea::TextArea;

use crate::config::Config;
use crate::event::{ev, EventHandler, Message};

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
struct State<'a> {
    tabs: IndexMap<String, Chat<'a>>,
    textarea_focused: bool,
    active_tab: Option<String>,
    textarea: TextArea<'a>,
    quit: bool,
    request_redraw: bool,
    mention_finder: memchr::memmem::Finder<'a>,
    handler_tx: UnboundedSender<ev::Send>,
    cfg: Config,
}

impl<'a> State<'a> {
    fn new(
        start_focused: bool,
        mention_finder: memchr::memmem::Finder<'a>,
        handler_tx: UnboundedSender<ev::Send>,
        cfg: Config,
    ) -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(Block::default().borders(Borders::ALL));
        Self {
            textarea_focused: start_focused,
            active_tab: None,
            tabs: IndexMap::default(),
            textarea,
            quit: false,
            mention_finder,
            request_redraw: false,
            handler_tx,
            cfg,
        }
    }

    fn key_event(&mut self, key: KeyEvent) {
        match key {
            KeyEvent {
                code: KeyCode::Char('q'),
                ..
            } => {
                if !self.textarea_focused {
                    self.quit = true;
                } else {
                    self.textarea.input(key);
                }
            }
            KeyEvent {
                code: KeyCode::Char('i'),
                ..
            } => {
                if !self.textarea_focused {
                    self.textarea_focused = true;
                } else {
                    self.textarea.input(key);
                }
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if self.textarea_focused {
                    self.textarea_focused = false;
                } else {
                    self.quit = true;
                }
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                if self.textarea_focused {
                    if let Some(channel) = &self.active_tab {
                        let text = self.textarea.lines().join(" ");
                        let message = Message {
                            channel: channel.clone(),
                            username: self.cfg.username.to_string(),
                            msg: text,
                        };
                        let _ = self.handler_tx.send(ev::Send::Message(message.clone()));

                        if let Some(tab) = self.tabs.get_mut(channel) {
                            tab.push_message(&self.mention_finder, message);
                        }
                    }
                }
            }
            _ => {
                if self.textarea_focused {
                    // TODO: abstract over crossterm Key
                    self.textarea.input(key);
                }
            }
        };

        self.request_redraw = true;
    }

    fn message_event(&mut self, message: Message) {
        if let Some(c) = self.tabs.get_mut(&message.channel) {
            if self
                .active_tab
                .as_ref()
                .is_some_and(|tab| tab == &message.channel)
            {
                self.request_redraw = true;
            }
            c.push_message(&self.mention_finder, message);
        }
    }
}

#[derive(Default)]
pub struct Tui;

impl Tui {
    pub async fn run(&mut self, cfg: Config, cache_dir: String) -> eyre::Result<()> {
        Self::init_error_hooks()?;
        let mut term = self.enter()?;

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (handler_tx, handler_rx) = mpsc::unbounded_channel();

        let mention_finder = memchr::memmem::Finder::new(cfg.username.as_ref());
        let mut state = State::new(false, mention_finder, handler_tx, cfg.clone());
        state.active_tab = Some(cfg.channels.first().map_or("".into(), |s| s.to_string()));

        state.tabs = IndexMap::from_iter(
            cfg.channels
                .iter()
                .map(|c| (c.clone().into_owned(), Chat::new())),
        );

        // Draw first frame early as possible
        term.draw(|frame| {
            Self::render(frame, &state);
        })?;

        {
            let cfg = cfg.clone();
            tokio::spawn(async move {
                let mut handler = EventHandler::new(cfg, cache_dir, event_tx, handler_rx);
                handler.run().await.unwrap();
            })
        };

        loop {
            select! {
                Some(e) = event_rx.recv() => {
                    match e {
                        ev::In::Key(k) => {
                            state.key_event(k);
                        }
                        ev::In::Message(message) => {
                            state.message_event(message);
                        }
                        ev::In::Redraw => state.request_redraw = true,
                    }
                }
            }

            if state.quit {
                break;
            }

            if state.request_redraw {
                term.draw(|frame| {
                    Self::render(frame, &state);
                })?;

                state.request_redraw = false;
            }
        }

        Self::leave()
    }

    fn init_error_hooks() -> eyre::Result<()> {
        let (panic, error) = HookBuilder::default().into_hooks();
        let panic = panic.into_panic_hook();
        let error = error.into_eyre_hook();
        eyre::set_hook(Box::new(move |e| {
            let _ = Self::leave();
            error(e)
        }))?;
        std::panic::set_hook(Box::new(move |e| {
            let _ = Self::leave();
            panic(e)
        }));
        Ok(())
    }

    fn enter(&self) -> eyre::Result<Terminal<CrosstermBackend<Stdout>>> {
        let backend = CrosstermBackend::new(std::io::stdout());
        let term = Terminal::new(backend)?;
        terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
        Ok(term)
    }

    fn leave() -> eyre::Result<()> {
        crossterm::execute!(std::io::stdout(), terminal::LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;
        Ok(())
    }

    fn render(frame: &mut Frame, state: &State) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(90), Constraint::Percentage(10)])
            .split(frame.area());

        frame.render_widget(&state.textarea, chunks[1]);
        let active = state.active_tab.clone().unwrap_or("".into());

        let mut tabs = Block::bordered().title_alignment(Alignment::Center);
        for (name, _) in &state.tabs {
            tabs = tabs.title(if name == &active {
                name.clone().into()
            } else {
                name.clone().dim()
            });
        }

        if let Some(active_chat) = state.tabs.get(&active) {
            frame.render_widget(active_chat.list(active).block(tabs), chunks[0]);
        } else {
            frame.render_widget(tabs, chunks[0]);
        }
    }
}
