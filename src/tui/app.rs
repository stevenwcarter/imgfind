use std::sync::Arc;

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
                AppEvent::Quit => self.quit(),
            },
        }
    }

    pub fn handle_zoom_image(&mut self, zoom: Option<u8>) {
        if self.zoomed_image_index == zoom {
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
                    let image = image.resize(800, 800, ratatui_image::FilterType::Triangle);
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
                KeyCode::Char('q') => self.events.send(AppEvent::Quit),
                KeyCode::Char('1') => self.events.send(AppEvent::ZoomImage(Some(0))),
                KeyCode::Char('2') => self.events.send(AppEvent::ZoomImage(Some(1))),
                KeyCode::Char('3') => self.events.send(AppEvent::ZoomImage(Some(2))),
                KeyCode::Char('4') => self.events.send(AppEvent::ZoomImage(Some(3))),
                KeyCode::Char('5') => self.events.send(AppEvent::ZoomImage(Some(4))),
                KeyCode::Char('6') => self.events.send(AppEvent::ZoomImage(Some(5))),
                KeyCode::Char('7') => self.events.send(AppEvent::ZoomImage(Some(6))),
                KeyCode::Char('8') => self.events.send(AppEvent::ZoomImage(Some(7))),
                KeyCode::Char('9') => self.events.send(AppEvent::ZoomImage(Some(8))),
                KeyCode::Esc => {
                    if self.zoomed_image_index.is_some() {
                        self.zoomed_image_index = None;
                    }
                }
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
