use anyhow::{Context, Result};
use dirs::home_dir;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sort::{Sort, SortDir, SortKey};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    /// Number of images to embed per CLIP batch during indexing
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

fn default_batch_size() -> usize {
    32
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            batch_size: default_batch_size(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Maximum cosine distance to include in results (lower = stricter)
    #[serde(default = "default_distance_threshold")]
    pub distance_threshold: f32,
    /// Upper bound on the vec0 `k` value (result-set ceiling)
    #[serde(default = "default_max_k")]
    pub max_k: usize,
}

fn default_distance_threshold() -> f32 {
    1.3
}

fn default_max_k() -> usize {
    100
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            distance_threshold: default_distance_threshold(),
            max_k: default_max_k(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    /// Number of adjacent images to preload in the lightbox (prev/next)
    #[serde(default = "default_preload_neighbors")]
    pub preload_neighbors: usize,
    /// Default sort key for the image grid
    #[serde(default = "default_gui_sort")]
    pub default_sort: SortKey,
    /// Default sort direction for the image grid
    #[serde(default = "default_gui_sort_dir")]
    pub default_sort_direction: SortDir,
}

fn default_preload_neighbors() -> usize {
    2
}

fn default_gui_sort() -> SortKey {
    SortKey::Name
}

fn default_gui_sort_dir() -> SortDir {
    SortDir::Asc
}

impl Default for GuiConfig {
    fn default() -> Self {
        GuiConfig {
            preload_neighbors: default_preload_neighbors(),
            default_sort: default_gui_sort(),
            default_sort_direction: default_gui_sort_dir(),
        }
    }
}

impl GuiConfig {
    /// Compose the stored key + direction into a `Sort` value.
    pub fn resolved_sort(&self) -> Sort {
        Sort {
            key: self.default_sort,
            dir: self.default_sort_direction,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directory patterns to ignore during indexing
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    /// Indexing tuning options
    #[serde(default)]
    pub index: IndexConfig,
    /// Search tuning options
    #[serde(default)]
    pub search: SearchConfig,
    /// Native GUI tuning options
    #[serde(default)]
    pub gui: GuiConfig,
    /// Default embedding model used to seed a newly-created database. `None`
    /// falls back to the built-in baseline model.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Cached compiled regexes (not serialized)
    #[serde(skip)]
    compiled_regexes: OnceCell<Vec<Option<Regex>>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            index: IndexConfig::default(),
            search: SearchConfig::default(),
            gui: GuiConfig::default(),
            default_model: None,
            ignore_patterns: vec![
                // Default patterns to ignore
                "node_modules".to_string(),
                ".git".to_string(),
                ".svn".to_string(),
                ".hg".to_string(),
                "target".to_string(),
                "build".to_string(),
                "dist".to_string(),
                ".cache".to_string(),
                "tmp".to_string(),
                "temp".to_string(),
            ],
            compiled_regexes: OnceCell::new(),
        }
    }
}

impl Config {
    /// Load configuration from ~/.imgfind/config.toml
    pub fn load() -> Result<Self> {
        let config_path = Self::get_config_path()?;

        if !config_path.exists() {
            // Create default config file if it doesn't exist
            let default_config = Self::default();
            default_config.save()?;
            return Ok(default_config);
        }

        let config_content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

        let config: Config = toml::from_str(&config_content)
            .with_context(|| format!("Failed to parse config file: {}", config_path.display()))?;

        Ok(config)
    }

    /// Save configuration to ~/.imgfind/config.toml
    pub fn save(&self) -> Result<()> {
        let config_path = Self::get_config_path()?;

        // Ensure the directory exists
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }

        let config_content =
            toml::to_string_pretty(self).context("Failed to serialize config to TOML")?;

        fs::write(&config_path, config_content)
            .with_context(|| format!("Failed to write config file: {}", config_path.display()))?;

        Ok(())
    }

    /// Get the path to the configuration file
    pub fn get_config_path() -> Result<PathBuf> {
        let home = home_dir().context("Could not find home directory")?;
        Ok(home.join(".imgfind").join("config.toml"))
    }

    /// Get or compile the regexes for ignore patterns
    fn get_compiled_regexes(&self) -> &Vec<Option<Regex>> {
        self.compiled_regexes.get_or_init(|| {
            self.ignore_patterns
                .iter()
                .map(|pattern| Regex::new(pattern).ok())
                .collect()
        })
    }

    /// Check if a path should be ignored based on the configured patterns
    pub fn should_ignore_path(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        let compiled_regexes = self.get_compiled_regexes();

        for (i, pattern) in self.ignore_patterns.iter().enumerate() {
            // Use compiled regex if available, otherwise fallback to string matching
            if let Some(regex) = &compiled_regexes[i] {
                if regex.is_match(&path_str) {
                    return true;
                }
            } else {
                // Fallback to simple string matching if regex is invalid
                if path_str.contains(pattern) {
                    return true;
                }
            }

            // Also check individual path components
            for component in path.components() {
                let component_str = component.as_os_str().to_string_lossy();
                if let Some(regex) = &compiled_regexes[i] {
                    if regex.is_match(&component_str) {
                        return true;
                    }
                } else if component_str.contains(pattern) {
                    return true;
                }
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn missing_gui_section_uses_defaults() {
        let cfg: Config = toml::from_str("ignore_patterns = []").unwrap();
        assert_eq!(cfg.gui.preload_neighbors, 2);
        assert_eq!(cfg.gui.default_sort, crate::sort::SortKey::Name);
        assert_eq!(cfg.gui.default_sort_direction, crate::sort::SortDir::Asc);
    }

    #[test]
    fn explicit_gui_values_parse() {
        let cfg: Config = toml::from_str(
            "[gui]\npreload_neighbors = 5\ndefault_sort = \"size\"\ndefault_sort_direction = \"desc\"\n",
        )
        .unwrap();
        assert_eq!(cfg.gui.preload_neighbors, 5);
        assert_eq!(cfg.gui.default_sort, crate::sort::SortKey::Size);
        assert_eq!(cfg.gui.default_sort_direction, crate::sort::SortDir::Desc);
        assert_eq!(
            cfg.gui.resolved_sort(),
            crate::sort::Sort {
                key: crate::sort::SortKey::Size,
                dir: crate::sort::SortDir::Desc,
            }
        );
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(!config.ignore_patterns.is_empty());
        assert!(config.ignore_patterns.contains(&"node_modules".to_string()));
    }

    #[test]
    fn test_should_ignore_path() {
        let config = Config {
            ignore_patterns: vec![
                "node_modules".to_string(),
                r".*generated.*".to_string(),
                ".git".to_string(),
            ],
            index: IndexConfig::default(),
            search: SearchConfig::default(),
            gui: GuiConfig::default(),
            default_model: None,
            compiled_regexes: OnceCell::new(),
        };

        // Test exact matches
        assert!(config.should_ignore_path(Path::new("/path/to/node_modules/file.jpg")));
        assert!(config.should_ignore_path(Path::new("/path/to/.git/file.jpg")));

        // Test regex patterns
        assert!(config.should_ignore_path(Path::new("/path/to/generated_files/file.jpg")));
        assert!(config.should_ignore_path(Path::new("/path/to/auto-generated/file.jpg")));

        // Test non-matching paths
        assert!(!config.should_ignore_path(Path::new("/path/to/images/file.jpg")));
        assert!(!config.should_ignore_path(Path::new("/path/to/photos/file.jpg")));
    }

    #[test]
    fn test_serialization() {
        let config = Config {
            ignore_patterns: vec!["node_modules".to_string(), r".*generated.*".to_string()],
            index: IndexConfig::default(),
            search: SearchConfig::default(),
            gui: GuiConfig::default(),
            default_model: None,
            compiled_regexes: OnceCell::new(),
        };

        let toml_str = toml::to_string(&config).unwrap();
        let deserialized: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(config.ignore_patterns, deserialized.ignore_patterns);
    }

    #[test]
    fn default_model_defaults_to_none_and_roundtrips() {
        // Absent from TOML -> None.
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        assert_eq!(cfg.default_model, None);

        // Set value survives a serialize/deserialize round-trip.
        let cfg = Config {
            default_model: Some("laion/CLIP-ViT-L-14-laion2B-s32B-b82K".to_string()),
            ..Default::default()
        };
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(
            back.default_model.as_deref(),
            Some("laion/CLIP-ViT-L-14-laion2B-s32B-b82K")
        );
    }

    #[test]
    fn test_default_index_batch_size() {
        assert_eq!(IndexConfig::default().batch_size, 32);
    }

    #[test]
    fn test_empty_toml_defaults_batch_size() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.index.batch_size, 32);
    }

    #[test]
    fn search_config_has_sane_defaults() {
        let sc = SearchConfig::default();
        assert_eq!(sc.distance_threshold, 1.3);
        assert_eq!(sc.max_k, 100);
    }

    #[test]
    fn config_defaults_include_search_section() {
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        assert_eq!(cfg.search.distance_threshold, 1.3);
        assert_eq!(cfg.search.max_k, 100);
    }

    #[test]
    fn config_roundtrips_search_section() {
        let mut cfg = Config::default();
        cfg.search.distance_threshold = 1.1;
        cfg.search.max_k = 50;
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.search.distance_threshold, 1.1);
        assert_eq!(back.search.max_k, 50);
    }
}
