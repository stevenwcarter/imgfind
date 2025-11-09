use crate::database::Database;
use crate::search::{SearchEngine, normalize_vector};
use crate::tui::event::Event;

use super::event::{AppEvent, EventHandler};
use anyhow::{Context, Result};
use clipper::ClipEmbedder;
use futures::FutureExt;
use image::ImageReader;
use ratatui::crossterm::event::Event as CrosstermEvent;
use ratatui::{
    DefaultTerminal,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
};
use ratatui_image::{
    picker::Picker,
    thread::{ResizeRequest, ThreadProtocol},
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::{
    select,
    sync::mpsc::{UnboundedReceiver, unbounded_channel},
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

/// Application.
pub struct App {
    pub db: Database,
    pub picker: Picker,
    pub images: Vec<ImageEntry>,
    /// Is the application running?
    pub running: bool,
    /// Counter.
    pub counter: u8,
    pub input: Input,
    pub input_mode: InputMode,
    /// Event handler.
    pub events: EventHandler,
    pub rx: UnboundedReceiver<ResizeRequest>,
    pub tx: UnboundedSender<ResizeRequest>,
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
        let (tx, rx) = unbounded_channel();
        let picker = Picker::from_query_stdio().unwrap();

        Ok(Self {
            db,
            images: Vec::new(),
            input: Input::default(),
            input_mode: InputMode::Normal,
            picker,
            running: true,
            counter: 0,
            events: EventHandler::new(),
            rx,
            tx,
            // image,
        })
    }

    /// Run the application's main loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        while self.running {
            // Handle resize requests for all images first
            self.handle_image_resize_requests();

            terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;

            select! {
                Ok(event) = self.events.next().fuse() => self.handle_event(event),
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

    pub fn handle_event(&mut self, event: Event) {
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
                    let e = self.handle_search(&query);
                    if let Err(err) = e {
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
        match key_event.code {
            KeyCode::Esc | KeyCode::Char('q') => self.events.send(AppEvent::Quit),
            KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Quit)
            }
            // Other handlers you could add here.
            _ => {}
        }
    }

    pub fn handle_search(&mut self, query: &str) -> Result<()> {
        self.images.clear();
        // Here you would implement the logic to handle the search query.
        // For example, you might want to use the picker to search for images.
        // For now, we'll just print the query to the console.
        // info!("Searching for: \"{}\"", query);

        // Get current directory for filtering results (not used in this simplified version)
        let _current_dir = std::env::current_dir().context("Failed to get current directory")?;

        // Check if database has any images
        let total_images = self.db.get_image_count()?;
        if total_images == 0 {
            return Ok(());
        }

        // info!("Loading CLIP model...");
        let model =
            ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?;

        // Generate embedding for query
        // info!("Generating embedding for query...");
        let query_embedding = model
            .get_text_embedding(query)
            .context("Failed to generate text embedding")?;

        let normalized_query = normalize_vector(&query_embedding);

        // Search database
        // info!("Searching database...");
        let search_engine = SearchEngine::new(&self.db);
        let all_results = search_engine.search(&normalized_query, usize::MAX)?; // Get all results first

        // Filter results based on current directory and recursive flag
        let filtered_results: Vec<_> = all_results
            .into_iter()
            .filter(|(_path, _score)| {
                // For now, just include all results (you can add filtering logic later)
                true
            })
            .take(9)
            .collect();

        if filtered_results.is_empty() {
            // if !short {
            //     if recursive {
            //         println!(
            //             "No images found matching the query \"{}\" in current directory or subdirectories.",
            //             prompt
            //         );
            //     } else {
            //         println!(
            //             "No images found matching the query \"{}\" in current directory.",
            //             prompt
            //         );
            //     }
            //     println!(
            //         "Try using --recursive to search subdirectories, or run 'imgfind index' to index current directory."
            //     );
            // }
            return Ok(());
        }

        for (path, score) in filtered_results.iter() {
            // info!("{:3}. {:<60} (similarity: {:.4})", i + 1, path, score);
            let image = ImageReader::open(path)
                .with_context(|| format!("Failed to open image at path: {}", path))?
                .decode()
                .with_context(|| format!("Failed to decode image at path: {}", path))?;

            // Create a separate channel for each image's resize requests
            let (image_tx, image_rx) = unbounded_channel();
            let protocol = self.picker.new_resize_protocol(image);

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

    /// Handles the tick event of the terminal.
    ///
    /// The tick event is where you can update the state of your application with any logic that
    /// needs to be updated at a fixed frame rate. E.g. polling a server, updating an animation.
    pub fn tick(&self) {}

    /// Set running to false to quit the application.
    pub fn quit(&mut self) {
        self.running = false;
    }

    pub fn increment_counter(&mut self) {
        self.counter = self.counter.saturating_add(1);
    }

    pub fn decrement_counter(&mut self) {
        self.counter = self.counter.saturating_sub(1);
    }
}
