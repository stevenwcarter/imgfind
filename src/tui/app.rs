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

/// Application.
pub struct App {
    pub db: Database,
    pub picker: Picker,
    pub protocol: ThreadProtocol,
    pub images: Vec<(String, f32, ThreadProtocol)>,
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
    // pub image: StatefulImage<StatefulProtocol>,
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
        let image = ImageReader::open("./test-images/test.jpg")?.decode()?;
        let picker = Picker::from_query_stdio().unwrap();

        let protocol = picker.new_resize_protocol(image);
        Ok(Self {
            db,
            images: Vec::new(),
            input: Input::default(),
            input_mode: InputMode::Normal,
            picker,
            protocol: ThreadProtocol::new(tx.clone(), Some(protocol)),
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
            terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;

            select! {
                Ok(event) = self.events.next().fuse() => self.handle_event(event),
                Some(request) = self.rx.recv().fuse() => self.handle_request(request)?,
            }
        }
        Ok(())
    }

    pub fn handle_request(&mut self, request: ResizeRequest) -> Result<()> {
        self.protocol
            .update_resized_protocol(request.resize_encode()?);
        Ok(())
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
                AppEvent::Increment => self.increment_counter(),
                AppEvent::Decrement => self.decrement_counter(),
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
                KeyCode::Right => self.events.send(AppEvent::Increment),
                KeyCode::Left => self.events.send(AppEvent::Decrement),
                _ => {}
            },
            InputMode::Editing => match key_event.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Enter => {
                    // Handle the input submission here.
                    self.events
                        .send(AppEvent::HandleSearch(self.input.value().to_string()));
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
            KeyCode::Right => self.events.send(AppEvent::Increment),
            KeyCode::Left => self.events.send(AppEvent::Decrement),
            // Other handlers you could add here.
            _ => {}
        }
    }

    pub fn handle_search(&mut self, query: &str) -> Result<()> {
        eprintln!("Handling search for query: {}", query);
        self.images.clear();
        // Here you would implement the logic to handle the search query.
        // For example, you might want to use the picker to search for images.
        // For now, we'll just print the query to the console.
        // info!("Searching for: \"{}\"", query);

        // Get current directory for filtering results
        let current_dir = std::env::current_dir().context("Failed to get current directory")?;

        // Check if database has any images
        let total_images = self.db.get_image_count()?;
        eprintln!("Total images in database: {}", total_images);
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

        eprintln!("Total results found: {}", all_results.len());

        // Filter results based on current directory and recursive flag
        let filtered_results: Vec<_> = all_results
            .into_iter()
            .filter(|(path, _score)| {
                let path_buf = std::path::Path::new(path);

                // The paths returned from the database are already absolute paths
                let abs_path = path_buf.to_path_buf();

                // Canonicalize paths to handle . and .. components and get absolute paths
                let abs_path = abs_path.canonicalize().unwrap_or(abs_path);
                let current_dir_canonical = current_dir
                    .canonicalize()
                    .unwrap_or_else(|_| current_dir.clone());

                true
                // if all {
                //     // For --all flag, include all results regardless of location
                //     true
                // } else if recursive {
                //     // For recursive search, check if the image is in current directory or any subdirectory
                //     abs_path.starts_with(&current_dir_canonical)
                // } else {
                //     // For non-recursive search, check if the image is directly in current directory
                //     if let Some(parent) = abs_path.parent() {
                //         parent == current_dir_canonical
                //     } else {
                //         false
                //     }
                // }
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

        for (i, (path, score)) in filtered_results.iter().enumerate() {
            // info!("{:3}. {:<60} (similarity: {:.4})", i + 1, path, score);
            let image = ImageReader::open(path)
                .with_context(|| format!("Failed to open image at path: {}", path))?
                .decode()
                .with_context(|| format!("Failed to decode image at path: {}", path))?;
            let protocol = self.picker.new_resize_protocol(image);
            self.images.push((
                path.clone(),
                *score,
                ThreadProtocol::new(self.tx.clone(), Some(protocol)),
            ));
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
