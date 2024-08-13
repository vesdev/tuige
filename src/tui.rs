use std::{collections::VecDeque, io::Stdout};

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
use tokio::{select, sync::mpsc};
use tui_textarea::TextArea;

use crate::{
    config::Config,
    event::{Event, EventHandler, Message},
};

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
        let mut handler = EventHandler::new(cfg.clone(), event_tx, handler_rx);

        tokio::spawn(async move {
            handler.run().await.unwrap();
        });

        let mention_finder = memchr::memmem::Finder::new(cfg.username.as_ref());

        let mut redraw = false;
        loop {
            select! {
                Some(e) = event_rx.recv() => {
                    match e {
                        Event::Key(k) => {
                            match k {
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
                            };

                            redraw = true;
                        }
                        Event::Message(message) => {
                            if let Some(c) = self.tabs.get_mut(&message.channel) {
                                if self.active_tab
                                    .as_ref()
                                    .is_some_and(|tab| tab == &message.channel) {
                                    redraw = true;
                                }
                                c.push_message(&mention_finder, message);
                            }
                        }
                        Event::Redraw => redraw = true,

                    }
                }
            }

            if redraw {
                term.draw(|frame| {
                    self.render(frame);
                })?;
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
