use super::event::{AppEvent, EventHandler};
use crate::database::Database;
use crate::tui::event::Event;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
pub use input::InputMode;
pub use search::ImageEntry;

use anyhow::{Context, Result};
use futures::FutureExt;
use ratatui::crossterm::event::Event as CrosstermEvent;
use ratatui::{
    DefaultTerminal,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::Rect,
};
use ratatui_image::picker::Picker;
use tokio::{
    select,
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
};
use tracing::{error, info};
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler as _;

mod focus;
mod input;
mod search;
mod zoom;

pub use focus::FocusDirection;
pub use search::SearchResult;
use zoom::focal_for_zoom_at_cursor;

/// Single source of truth for the help overlay. One entry per binding.
pub fn keybindings_help() -> Vec<String> {
    vec![
        "e          edit search".into(),
        "h/j/k/l    move focus".into(),
        "H/L        previous / next page".into(),
        "1-9        zoom that image".into(),
        "Enter      zoom focused image".into(),
        "Esc        close zoom / help".into(),
        "scroll     zoom in/out (in zoom view)".into(),
        "right-click reset zoom".into(),
        "?          toggle this help".into(),
        "q / Ctrl-C quit".into(),
    ]
}

/// Application.
pub struct App {
    /// Handle to the database for performing queries
    pub db: Database,
    /// Picker which identifies the capabilities of the current terminal
    pub picker: Picker,
    /// Images found from a search result
    pub images: Vec<ImageEntry>,
    /// Is the application running?
    pub running: bool,
    /// Which index is currently zoomed
    pub zoomed_image_index: Option<u8>,
    /// Larger version of the image which should be displayed as zoomed if present
    pub zoomed_image: Option<ImageEntry>,
    pub zoom_level: u8,
    /// Crop-window center for the zoomed image, in original-image normalized [0,1] coords.
    pub zoom_focal: (f32, f32),
    /// On-screen rect of the displayed zoomed image, captured during render.
    pub zoomed_image_rect: Option<Rect>,
    /// Which image index is currently focused
    pub focused_image_index: u8,
    /// Holds the input component/state
    pub input: Input,
    /// Which page of search results is currently being displayed
    pub page: usize,
    /// Determines whether we're editing the search box or not
    pub input_mode: InputMode,
    /// What the last search string was
    pub last_search: Option<String>,
    /// The results for the latest search
    pub search_result: Option<SearchResult>,
    /// Event handler.
    pub events: EventHandler,
    /// Channel to receive search results
    pub search_rx: UnboundedReceiver<SearchResult>,
    /// Channel to send search results with
    pub search_tx: UnboundedSender<SearchResult>,
    /// Channel to receive zoom results from image decoding
    pub zoom_rx: UnboundedReceiver<ImageEntry>,
    /// Channel to send zoom results to
    pub zoom_tx: UnboundedSender<ImageEntry>,
    /// Current search task (if any)
    pub current_search_task: Option<JoinHandle<()>>,
    /// Loading state
    pub is_searching: bool,
    pub mouse_click: Option<MouseEvent>,
    /// Whether the keybindings help overlay is shown
    pub show_help: bool,
}

impl App {
    /// Constructs a new instance of [`App`].
    pub fn new(db: Database) -> Result<Self> {
        let picker = Picker::from_query_stdio().context("could not determine tui environment")?;
        let (search_tx, search_rx) = unbounded_channel();
        let (zoom_tx, zoom_rx) = unbounded_channel();

        Ok(Self {
            db,
            focused_image_index: 0,
            images: Vec::new(),
            input: Input::default(),
            input_mode: InputMode::Normal,
            picker,
            running: true,
            page: 0,
            zoom_level: 1,
            zoom_focal: (0.5, 0.5),
            zoomed_image_rect: None,
            zoomed_image_index: None,
            zoomed_image: None,
            search_result: None,
            events: EventHandler::default(),
            last_search: None,
            search_rx,
            search_tx,
            zoom_tx,
            zoom_rx,
            current_search_task: None,
            is_searching: false,
            mouse_click: None,
            show_help: false,
        })
    }

    /// Run the application's main loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        while self.running {
            // Handle resize requests for all images first
            self.handle_image_resize_requests();

            terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;

            select! {
                Ok(event) = self.events.next().fuse() => self.handle_event(event).await,
                Some(image_entry) = self.zoom_rx.recv().fuse() => {
                    self.zoomed_image = Some(image_entry);
                },
                Some(search_result) = self.search_rx.recv().fuse() => {
                    self.handle_search_result(search_result)?;
                }
            }
        }
        Ok(())
    }

    pub async fn handle_event(&mut self, event: Event) {
        match event {
            Event::Tick => self.tick(),
            Event::Crossterm(event) => match event {
                crossterm::event::Event::Mouse(mouse_event) => self.handle_mouse_event(mouse_event),
                crossterm::event::Event::Key(key_event)
                    if key_event.kind == crossterm::event::KeyEventKind::Press =>
                {
                    self.handle_key_events(key_event)
                }
                _ => {}
            },
            Event::App(app_event) => match app_event {
                AppEvent::ZoomIn(mouse_event) => {
                    if self.zoomed_image.is_some() {
                        let new_zoom = self.zoom_level.saturating_add(1).clamp(1, 4);
                        if let Some(rect) = self.zoomed_image_rect {
                            self.zoom_focal = focal_for_zoom_at_cursor(
                                rect,
                                mouse_event.column,
                                mouse_event.row,
                                self.zoom_level,
                                self.zoom_focal,
                                new_zoom,
                            );
                        }
                        self.zoom_level = new_zoom;
                        self.handle_zoom_image(self.zoomed_image_index);
                    }
                }
                AppEvent::ZoomOut(mouse_event) => {
                    if self.zoomed_image.is_some() {
                        let new_zoom = self.zoom_level.saturating_sub(1).clamp(1, 4);
                        if let Some(rect) = self.zoomed_image_rect {
                            self.zoom_focal = focal_for_zoom_at_cursor(
                                rect,
                                mouse_event.column,
                                mouse_event.row,
                                self.zoom_level,
                                self.zoom_focal,
                                new_zoom,
                            );
                        }
                        self.zoom_level = new_zoom;
                        self.handle_zoom_image(self.zoomed_image_index);
                    }
                }
                AppEvent::ZoomReset => {
                    self.zoom_level = 1;
                    self.zoom_focal = (0.5, 0.5);
                    self.handle_zoom_image(self.zoomed_image_index);
                }
                AppEvent::ClickLeft(mouse_event) => {
                    if self.zoomed_image.is_some() {
                        self.mouse_click = None;
                        self.events.send(AppEvent::ZoomImage(None))
                    } else {
                        self.mouse_click = Some(mouse_event);
                    }
                }
                AppEvent::HandleSearch(query) => {
                    self.page = 0;
                    if let Err(err) = self.handle_search(&query) {
                        error!("Error handling search: {:?}", err);
                    }
                }
                AppEvent::NextPage => {
                    if let Some(result) = &self.search_result
                        && (self.page + 1) * 9 >= result.result_count
                    {
                        // No more pages
                        return;
                    }
                    self.page += 1;
                    if let Err(err) = self.update_page() {
                        error!("Error updating page: {:?}", err);
                    }
                }
                AppEvent::PreviousPage => {
                    self.page = self.page.saturating_sub(1);
                    if let Err(err) = self.update_page() {
                        error!("Error updating page: {:?}", err);
                    }
                }
                AppEvent::ZoomImage(zoom) => {
                    self.zoom_focal = (0.5, 0.5);
                    self.handle_zoom_image(zoom);
                }
                AppEvent::Focus(direction) => self.handle_focus(direction),
                AppEvent::Quit => self.quit(),
            },
        }
    }

    pub fn handle_mouse_event(&mut self, mouse_event: MouseEvent) {
        info!("Received mouse event: {:?}", mouse_event);
        match mouse_event.kind {
            MouseEventKind::ScrollUp => {
                if self.zoomed_image.is_some() {
                    self.events.send(AppEvent::ZoomIn(mouse_event))
                }
            }
            MouseEventKind::ScrollDown => {
                if self.zoomed_image.is_some() {
                    self.events.send(AppEvent::ZoomOut(mouse_event))
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.events.send(AppEvent::ClickLeft(mouse_event))
            }
            MouseEventKind::Up(MouseButton::Right) => {
                if self.zoomed_image.is_some() {
                    self.events.send(AppEvent::ZoomReset);
                }
            }
            e => {
                info!("Other mouse event: {:?}", e);
            }
        }
    }

    /// Handles the key events and updates the state of [`App`].
    pub fn handle_key_events(&mut self, key_event: KeyEvent) {
        use FocusDirection::*;

        // While the help overlay is open it is modal: only quit, Esc, and '?'
        // are honored; everything else is swallowed so navigation is blocked.
        if self.show_help && self.input_mode == InputMode::Normal {
            match key_event.code {
                KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                    self.events.send(AppEvent::Quit)
                }
                KeyCode::Char('q') => self.events.send(AppEvent::Quit),
                KeyCode::Esc | KeyCode::Char('?') => self.show_help = false,
                _ => {}
            }
            return;
        }

        match self.input_mode {
            InputMode::Normal => match key_event.code {
                KeyCode::Char('?') => self.show_help = !self.show_help,
                KeyCode::Char('e') => {
                    self.input_mode = InputMode::Editing;
                }
                KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                    self.events.send(AppEvent::Quit)
                }
                KeyCode::Char('L') => self.events.send(AppEvent::NextPage),
                KeyCode::Char('H') => self.events.send(AppEvent::PreviousPage),
                KeyCode::Char('l') => self.events.send(AppEvent::Focus(Right)),
                KeyCode::Char('k') => self.events.send(AppEvent::Focus(Up)),
                KeyCode::Char('j') => self.events.send(AppEvent::Focus(Down)),
                KeyCode::Char('h') => self.events.send(AppEvent::Focus(Left)),
                KeyCode::Char('q') => self.events.send(AppEvent::Quit),
                KeyCode::Enter => {
                    if self.zoomed_image_index.is_some() {
                        self.events.send(AppEvent::ZoomImage(None));
                    } else {
                        self.events
                            .send(AppEvent::ZoomImage(Some(self.focused_image_index)));
                    }
                }
                KeyCode::Char(c @ '1'..='9') => {
                    // subtract the byte representation of '1' to get zero-based index
                    let idx = c as u8 - b'1';
                    self.events.send(AppEvent::ZoomImage(Some(idx)));
                }
                KeyCode::Esc => self.events.send(AppEvent::ZoomImage(None)),
                _ => {}
            },
            InputMode::Editing => match key_event.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Enter => {
                    // Handle the input submission here.
                    self.input_mode = InputMode::Normal;
                    self.events
                        .send(AppEvent::HandleSearch(self.input.value().to_string()));
                    self.input.reset();
                }
                _ => {
                    self.input.handle_event(&CrosstermEvent::Key(key_event));
                }
            },
        }
    }

    /// Handles the tick event of the terminal.
    ///
    /// The tick event is where you can update the state of your application with any logic that
    /// needs to be updated at a fixed frame rate. E.g. polling a server, updating an animation.
    pub fn tick(&self) {}

    /// Set running to false to quit the application.
    pub fn quit(&mut self) {
        self.running = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keybindings_help_lists_core_keys() {
        let lines = keybindings_help();
        assert!(!lines.is_empty());
        let joined = lines.join(" ");
        for key in ["e", "h/j/k/l", "H/L", "1-9", "Enter", "Esc", "?", "q"] {
            assert!(joined.contains(key), "help should mention {key}");
        }
    }
}
