use anyhow::{Context, Result};
use dirs::home_dir;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directory patterns to ignore during indexing
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    /// Cached compiled regexes (not serialized)
    #[serde(skip)]
    compiled_regexes: OnceCell<Vec<Option<Regex>>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
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
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
        }

        let config_content = toml::to_string_pretty(self)
            .context("Failed to serialize config to TOML")?;

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
            ignore_patterns: vec![
                "node_modules".to_string(),
                r".*generated.*".to_string(),
            ],
            compiled_regexes: OnceCell::new(),
        };

        let toml_str = toml::to_string(&config).unwrap();
        let deserialized: Config = toml::from_str(&toml_str).unwrap();
        
        assert_eq!(config.ignore_patterns, deserialized.ignore_patterns);
    }
}
