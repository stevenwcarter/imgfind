use crate::database::Database;
use crate::search::{SearchEngine, normalize_vector};
use crate::tui::event::Event;

use super::event::{AppEvent, EventHandler};
use anyhow::{Context, Result};
use clipper::ClipEmbedder;
use futures::FutureExt;
use image::{DynamicImage, ImageReader, load_from_memory};
use ratatui::crossterm::event::Event as CrosstermEvent;
use ratatui::{
    DefaultTerminal,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
};
use ratatui_image::{
    picker::Picker,
    thread::{ResizeRequest, ThreadProtocol},
};
use tokio::{
    select,
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
};
use tracing::{debug, error};
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler as _;

/// Represents an image with its associated data and protocol
pub struct ImageEntry {
    pub path: String,
    pub score: f32,
    pub protocol: ThreadProtocol,
    pub rx: UnboundedReceiver<ResizeRequest>,
}

#[derive(Debug, Clone, Copy)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}
use FocusDirection::{Down, Left, Right, Up};

/// Search result from background task
#[derive(Clone)]
pub struct SearchResult {
    pub images: Vec<(String, f32, DynamicImage)>,
    pub result_count: usize,
    pub query: String,
}

/// Application.
pub struct App {
    pub db: Database,
    pub picker: Picker,
    pub images: Vec<ImageEntry>,
    /// Is the application running?
    pub running: bool,
    pub zoomed_image_index: Option<u8>,
    pub zoomed_image: Option<ImageEntry>,
    pub focused_image_index: u8,
    pub input: Input,
    pub page: usize,
    pub input_mode: InputMode,
    pub result_count: usize,
    pub last_search: Option<String>,
    pub search_result: Option<SearchResult>,
    /// Event handler.
    pub events: EventHandler,
    /// Channel to receive search results
    pub search_rx: UnboundedReceiver<SearchResult>,
    pub search_tx: UnboundedSender<SearchResult>,
    pub zoom_rx: UnboundedReceiver<ImageEntry>,
    pub zoom_tx: UnboundedSender<ImageEntry>,
    /// Current search task (if any)
    pub current_search_task: Option<JoinHandle<()>>,
    /// Loading state
    pub is_searching: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    #[default]
    Normal,
    Editing,
}

impl App {
    /// Constructs a new instance of [`App`].
    pub fn new(db: Database) -> Result<Self> {
        let picker = Picker::from_query_stdio().unwrap();
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
            result_count: 0,
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
                    debug!("Received zoomed image: {}", image_entry.path);
                    self.zoomed_image = Some(image_entry);
                },
                Some(search_result) = self.search_rx.recv().fuse() => {
                    self.handle_search_result(search_result)?;
                }
            }
        }
        Ok(())
    }

    /// Handle resize requests for all loaded images
    fn handle_image_resize_requests(&mut self) {
        if let Some(image_entry) = self.zoomed_image.as_mut()
            && let Ok(request) = image_entry.rx.try_recv()
            && let Ok(resized) = request.resize_encode()
        {
            debug!("Resizing image");
            image_entry.protocol.update_resized_protocol(resized);
        }
        for image_entry in &mut self.images {
            if let Ok(request) = image_entry.rx.try_recv()
                && let Ok(resized) = request.resize_encode()
            {
                image_entry.protocol.update_resized_protocol(resized);
            }
        }
    }

    /// Handle search results from background task
    fn handle_search_result(&mut self, search_result: SearchResult) -> Result<()> {
        self.is_searching = false;
        self.current_search_task = None;
        self.images.clear();
        self.zoomed_image = None;
        self.zoomed_image_index = None;
        self.result_count = search_result.result_count;
        self.search_result = Some(search_result);

        self.update_page()
    }

    fn update_page(&mut self) -> Result<()> {
        let result_count = self
            .search_result
            .as_ref()
            .map_or(0, |res| res.result_count);
        debug!(
            "Updating to page {}, with {} results",
            self.page, result_count
        );
        if self.search_result.is_none()
            || self.search_result.as_ref().unwrap().images.len() <= self.page * 9
        {
            error!(
                "End of results, have {}",
                self.search_result.as_ref().unwrap().images.len()
            );
            return Ok(());
        }
        self.images.clear();

        for (path, score, image) in self
            .search_result
            .as_ref()
            .context("grabbing search results")?
            .images
            .iter()
            .skip(self.page * 9)
            .take(9)
        {
            // Create a separate channel for each image's resize requests
            let (image_tx, image_rx) = unbounded_channel();
            let protocol = self.picker.new_resize_protocol(image.clone());

            let image_entry = ImageEntry {
                path: path.clone(),
                score: *score,
                protocol: ThreadProtocol::new(image_tx, Some(protocol)),
                rx: image_rx,
            };

            self.images.push(image_entry);
        }

        Ok(())
    }

    pub async fn handle_event(&mut self, event: Event) {
        match event {
            Event::Tick => self.tick(),
            Event::Crossterm(event) => match event {
                crossterm::event::Event::Key(key_event)
                    if key_event.kind == crossterm::event::KeyEventKind::Press =>
                {
                    self.handle_key_events(key_event)
                }
                _ => {}
            },
            Event::App(app_event) => match app_event {
                AppEvent::HandleSearch(query) => {
                    self.page = 0;
                    if let Err(err) = self.handle_search(&query) {
                        // Handle the error, e.g., log it or display a message to the user.
                        // For now, we'll just print it to the console.
                        eprintln!("Error handling search: {:?}", err);
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
                        eprintln!("Error updating page: {:?}", err);
                    }
                }
                AppEvent::PreviousPage => {
                    self.page = self.page.saturating_sub(1);
                    if let Err(err) = self.update_page() {
                        eprintln!("Error updating page: {:?}", err);
                    }
                }
                AppEvent::ZoomImage(zoom) => {
                    self.handle_zoom_image(zoom);
                }
                AppEvent::Focus(direction) => self.handle_focus(direction),
                AppEvent::Quit => self.quit(),
            },
        }
    }

    /// Handle focus movement in the image grid
    /// Focus wraps to the next/previous row when moving up/down
    pub fn handle_focus(&mut self, direction: FocusDirection) {
        let images_len = self.images.len() as u8;
        let current_index = self.focused_image_index;

        self.focused_image_index = calculate_new_focus_index(images_len, current_index, direction);
    }

    pub fn handle_zoom_image(&mut self, zoom: Option<u8>) {
        if self.zoomed_image_index == zoom || zoom.is_none() {
            self.zoomed_image_index = None;
            self.zoomed_image = None;
        } else {
            self.zoomed_image_index = zoom;
            if let Some(zoom_index) = zoom {
                let image_entry = self
                    .images
                    .get(zoom_index as usize)
                    .expect("image not found");
                let image_path = image_entry.path.clone();
                let image_score = image_entry.score;

                let zoom_tx = self.zoom_tx.clone();
                let picker = self.picker.clone();

                tokio::spawn(async move {
                    debug!("Image path is: {}", image_path);
                    let image = ImageReader::open(image_path.clone())
                        .expect("could not open")
                        .decode()
                        .expect("could not decoded");
                    // let image = image.resize(800, 800, ratatui_image::FilterType::Triangle);
                    debug!("Image decoded successfully");

                    let (image_tx, image_rx) = unbounded_channel();
                    let protocol = picker.new_resize_protocol(image);
                    let image_entry = ImageEntry {
                        path: image_path.clone(),
                        score: image_score,
                        rx: image_rx,
                        protocol: ThreadProtocol::new(image_tx, Some(protocol)),
                    };

                    zoom_tx
                        .send(image_entry)
                        .expect("Could not send image entry");
                });
            }
        }
    }

    /// Handles the key events and updates the state of [`App`].
    pub fn handle_key_events(&mut self, key_event: KeyEvent) {
        match self.input_mode {
            InputMode::Normal => match key_event.code {
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

    pub fn handle_search(&mut self, query: &str) -> Result<()> {
        self.last_search = Some(query.to_owned());
        // Cancel any existing search
        if let Some(task) = self.current_search_task.take() {
            task.abort();
        }

        // Set loading state
        self.is_searching = true;
        self.images.clear(); // Clear previous results immediately

        let db = self.db.clone();
        let query = query.to_string();
        let search_tx = self.search_tx.clone();

        let task = tokio::spawn(async move {
            let search_result: Result<SearchResult> = async {
                let mut images: Vec<(String, f32, DynamicImage)> = Vec::new();

                // Check if database has any images
                let total_images = db.get_image_count()?;
                if total_images == 0 {
                    return Ok(SearchResult {
                        images,
                        result_count: 0,
                        query: query.clone(),
                    });
                }

                // Load CLIP model
                let model = ClipEmbedder::new(None, None, false)
                    .context("Failed to create ClipEmbedder")?;

                // Generate embedding for query
                let query_embedding = model
                    .get_text_embedding(&query)
                    .context("Failed to generate text embedding")?;

                let normalized_query = normalize_vector(&query_embedding);

                // Search database
                let search_engine = SearchEngine::new(&db);
                let all_results =
                    search_engine.search_with_thumbnails_raw(&normalized_query, 99, 0)?;

                let result_count = all_results.len();

                // Filter results
                let filtered_results: Vec<_> = all_results
                    .into_iter()
                    .filter(|(_path, _score, _image)| true) // Add filtering logic as needed
                    // .skip(page * 9)
                    // .take(9)
                    .collect();

                if filtered_results.is_empty() {
                    return Ok(SearchResult {
                        images,
                        result_count,
                        query: query.clone(),
                    });
                }

                for (path, score, image) in filtered_results.iter() {
                    if let Some(image) = image {
                        let image = load_from_memory(image).with_context(|| {
                            format!("Failed to decode image blob for path: {}", path)
                        })?;
                        images.push((path.clone(), *score, image));
                    }
                }

                Ok(SearchResult {
                    images,
                    result_count,
                    query: query.clone(),
                })
            }
            .await;

            // Send result back to main thread
            match search_result {
                Ok(result) => {
                    let _ = search_tx.send(result);
                }
                Err(err) => {
                    eprintln!("Search error: {:?}", err);
                    // Send empty result on error
                    let _ = search_tx.send(SearchResult {
                        images: Vec::new(),
                        result_count: 0,
                        query: query.clone(),
                    });
                }
            }
        });

        self.current_search_task = Some(task);
        Ok(())
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

pub fn calculate_new_focus_index(
    images_len: u8,
    current_index: u8,
    direction: FocusDirection,
) -> u8 {
    let mut current_index = current_index;
    match direction {
        Left => {
            if current_index == 0 {
                current_index = images_len - 1;
            } else {
                current_index -= 1;
            }
        }
        Right => {
            current_index += 1;
            current_index %= images_len;
        }
        Up => {
            if current_index < 3 {
                let remainder = current_index % 3;
                let rows = (images_len - 1) / 3;
                current_index = (rows * 3) + remainder;
                if current_index >= images_len - 1 {
                    current_index = images_len - 1;
                }
            } else {
                current_index -= 3;
            }
        }
        Down => {
            if current_index + 3 >= images_len {
                current_index %= 3
            } else {
                let new_index = current_index + 3;
                if new_index >= images_len {
                    current_index = images_len - 1;
                } else {
                    current_index = new_index;
                }
            }
        }
    }

    current_index
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn it_calculates_correct_right_indexes() {
        assert_eq!(calculate_new_focus_index(9, 0, Right), 1);
        assert_eq!(calculate_new_focus_index(9, 8, Right), 0);
    }
    #[test]
    fn it_calculates_correct_left_indexes() {
        assert_eq!(calculate_new_focus_index(9, 0, Left), 8);
        assert_eq!(calculate_new_focus_index(9, 8, Left), 7);
    }
    #[test]
    fn it_calculates_correct_up_indexes() {
        assert_eq!(calculate_new_focus_index(9, 0, Up), 6);
        assert_eq!(calculate_new_focus_index(9, 1, Up), 7);
        assert_eq!(calculate_new_focus_index(9, 2, Up), 8);
        assert_eq!(calculate_new_focus_index(9, 3, Up), 0);
        assert_eq!(calculate_new_focus_index(9, 4, Up), 1);
        assert_eq!(calculate_new_focus_index(9, 5, Up), 2);
        assert_eq!(calculate_new_focus_index(9, 6, Up), 3);
        assert_eq!(calculate_new_focus_index(9, 7, Up), 4);
        assert_eq!(calculate_new_focus_index(9, 8, Up), 5);
        assert_eq!(calculate_new_focus_index(6, 0, Up), 3);
        assert_eq!(calculate_new_focus_index(6, 1, Up), 4);
        assert_eq!(calculate_new_focus_index(6, 2, Up), 5);
        assert_eq!(calculate_new_focus_index(8, 0, Up), 6);
        assert_eq!(calculate_new_focus_index(8, 1, Up), 7);
        assert_eq!(calculate_new_focus_index(8, 2, Up), 7);
        assert_eq!(calculate_new_focus_index(7, 0, Up), 6);
        assert_eq!(calculate_new_focus_index(7, 1, Up), 6);
        assert_eq!(calculate_new_focus_index(7, 2, Up), 6);
    }
    #[test]
    fn it_calculates_correct_down_indexes() {
        assert_eq!(calculate_new_focus_index(9, 0, Down), 3);
        assert_eq!(calculate_new_focus_index(9, 1, Down), 4);
        assert_eq!(calculate_new_focus_index(9, 2, Down), 5);
        assert_eq!(calculate_new_focus_index(9, 3, Down), 6);
        assert_eq!(calculate_new_focus_index(9, 4, Down), 7);
        assert_eq!(calculate_new_focus_index(9, 5, Down), 8);
        assert_eq!(calculate_new_focus_index(9, 6, Down), 0);
        assert_eq!(calculate_new_focus_index(9, 7, Down), 1);
        assert_eq!(calculate_new_focus_index(9, 8, Down), 2);
        assert_eq!(calculate_new_focus_index(6, 0, Down), 3);
        assert_eq!(calculate_new_focus_index(6, 1, Down), 4);
        assert_eq!(calculate_new_focus_index(6, 2, Down), 5);
        assert_eq!(calculate_new_focus_index(6, 3, Down), 0);
        assert_eq!(calculate_new_focus_index(6, 4, Down), 1);
        assert_eq!(calculate_new_focus_index(6, 5, Down), 2);
        assert_eq!(calculate_new_focus_index(8, 6, Down), 0);
        assert_eq!(calculate_new_focus_index(8, 7, Down), 1);
    }
}
