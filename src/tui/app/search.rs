use anyhow::{Context, Result};
use clipper::ClipEmbedder;
use image::{DynamicImage, load_from_memory};
use ratatui_image::thread::{ResizeRequest, ThreadProtocol};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tracing::{debug, error};

use crate::{
    search::{SearchEngine, normalize_vector},
    tui::app::App,
};

/// Search result from background task
#[derive(Clone)]
pub struct SearchResult {
    /// Vec of images returned from the search
    pub images: Vec<(String, f32, DynamicImage)>,
    /// Number of results found
    pub result_count: usize,
    /// The query that resulted in these search results
    pub query: String,
}

/// Represents an image with its associated data and protocol
pub struct ImageEntry {
    /// Filesystem path to the image
    pub path: String,
    /// Search score for the result
    pub score: f32,
    /// Protocol used to handle resize events
    pub protocol: ThreadProtocol,
    /// Listener for resize requests
    pub rx: UnboundedReceiver<ResizeRequest>,
}

impl App {
    /// Handle resize requests for all loaded images
    pub(crate) fn handle_image_resize_requests(&mut self) {
        if let Some(image_entry) = self.zoomed_image.as_mut()
            && let Ok(request) = image_entry.rx.try_recv()
            && let Ok(resized) = request.resize_encode()
        {
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

    pub(crate) fn result_count(&self) -> Option<usize> {
        Some(self.search_result.as_ref()?.result_count)
    }

    /// Handle search results from background task
    pub(crate) fn handle_search_result(&mut self, search_result: SearchResult) -> Result<()> {
        self.is_searching = false;
        self.current_search_task = None;
        self.images.clear();

        // clear previous zoomed state
        self.zoomed_image = None;
        self.zoomed_image_index = None;

        self.search_result = Some(search_result);

        self.update_page()
    }

    pub(crate) fn update_page(&mut self) -> Result<()> {
        let result_count = self.result_count().unwrap_or(0);
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

    pub(crate) fn handle_search(&mut self, query: &str) -> Result<()> {
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
}
