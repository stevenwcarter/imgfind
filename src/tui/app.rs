use crate::database::Database;
use crate::search::{SearchEngine, normalize_vector};
use crate::tui::event::Event;

use super::event::{AppEvent, EventHandler};
use anyhow::{Context, Result};
use clipper::ClipEmbedder;
use futures::FutureExt;
use image::{DynamicImage, ImageReader};
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
pub struct SearchResult {
    pub images: Vec<(String, f32, DynamicImage)>,
    pub query: String,
}

/// Application.
pub struct App {
    pub db: Database,
    pub picker: Picker,
    pub images: Vec<ImageEntry>,
    /// Is the application running?
    pub running: bool,
    pub input: Input,
    pub input_mode: InputMode,
    pub last_search: Option<String>,
    /// Event handler.
    pub events: EventHandler,
    /// Channel to receive search results
    pub search_rx: UnboundedReceiver<SearchResult>,
    pub search_tx: UnboundedSender<SearchResult>,
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

        Ok(Self {
            db,
            images: Vec::new(),
            input: Input::default(),
            input_mode: InputMode::Normal,
            picker,
            running: true,
            events: EventHandler::default(),
            last_search: None,
            search_rx,
            search_tx,
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
                Some(search_result) = self.search_rx.recv().fuse() => {
                    self.handle_search_result(search_result);
                }
            }
        }
        Ok(())
    }

    /// Handle resize requests for all loaded images
    fn handle_image_resize_requests(&mut self) {
        for image_entry in &mut self.images {
            if let Ok(request) = image_entry.rx.try_recv()
                && let Ok(resized) = request.resize_encode()
            {
                image_entry.protocol.update_resized_protocol(resized);
            }
        }
    }

    /// Handle search results from background task
    fn handle_search_result(&mut self, search_result: SearchResult) {
        self.is_searching = false;
        self.current_search_task = None;
        self.images.clear();

        for (path, score, image) in search_result.images.iter() {
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
                    if let Err(err) = self.handle_search(&query) {
                        // Handle the error, e.g., log it or display a message to the user.
                        // For now, we'll just print it to the console.
                        eprintln!("Error handling search: {:?}", err);
                    }
                }
                AppEvent::Quit => self.quit(),
            },
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
                KeyCode::Esc | KeyCode::Char('q') => self.events.send(AppEvent::Quit),
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
                let all_results = search_engine.search(&normalized_query, usize::MAX)?;

                // Filter results
                let filtered_results: Vec<_> = all_results
                    .into_iter()
                    .filter(|(_path, _score)| true) // Add filtering logic as needed
                    .take(9)
                    .collect();

                if filtered_results.is_empty() {
                    return Ok(SearchResult {
                        images,
                        query: query.clone(),
                    });
                }

                for (path, score) in filtered_results.iter() {
                    let image = ImageReader::open(path)
                        .with_context(|| format!("Failed to open image at path: {}", path))?
                        .decode()
                        .with_context(|| format!("Failed to decode image at path: {}", path))?;

                    images.push((path.clone(), *score, image));
                }

                Ok(SearchResult {
                    images,
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
