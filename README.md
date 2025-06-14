# `imgfind` – CLIP-Based Image Search CLI

## 🧭 Overview

`imgfind` is a Rust-based CLI tool that helps users find images in a directory (or recursively) that match a natural language query. It uses CLIP embeddings to compute semantic similarity between a user-provided prompt and indexed image content.

## ✨ Features

- **🔍 Natural Language Search**: Find images using descriptive text like "sunset over mountains" or "a cat sitting on a chair"
- **⚡ Fast Indexing**: Efficient image processing and embedding generation using CLIP
- **📊 Smart Caching**: Avoids re-processing unchanged images using content hashing
- **🗄️ SQLite Storage**: Reliable database storage with efficient vector similarity search
- **🔧 CLI Interface**: Simple command-line interface with helpful status information

## 🚀 Quick Start

### Installation

1. **Build from source:**

   ```bash
   cargo build --release
   ```

2. **Install locally:**

   ```bash
   ./install.sh
   ```

### Basic Usage

1. **Index your images:**

   ```bash
   imgfind index --dir ~/Pictures --recursive
   ```

2. **Search for images:**

   ```bash
   imgfind search "beach vacation"
   imgfind search "family dinner" --limit 5
   imgfind search "landscape photography"
   ```

3. **Check status:**

   ```bash
   imgfind status
   ```

4. **Clean up missing files:**

   ```bash
   imgfind clean
   ```

## 🛠️ Commands

| Command | Description | Options |
|---------|-------------|---------|
| `index` | Index images in a directory | `--dir <path>`, `--recursive` |
| `search` | Search using natural language | `--limit <number>` |
| `clean` | Remove entries for missing files | - |
| `status` | Show database statistics | - |

## 📂 Project Structure

### Technology Stack

- **Language**: Rust
- **Image/Text Embeddings**: Custom `clipper` crate (CLIP-based)
- **Hashing**: `oshash-rs` (media-optimized hashing)
- **Database**: SQLite via `rusqlite`
- **Vector Search**: Cosine similarity on normalized embeddings
- **CLI**: `clap` for argument parsing

### Database

- **Location**: `~/.imgfind/imgfind.db` (automatically searches up directory tree)
- **Schema**:

  ```sql
  CREATE TABLE images (
    id INTEGER PRIMARY KEY,
    path TEXT UNIQUE NOT NULL,
    hash TEXT NOT NULL,
    embedding BLOB NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
  );
  ```

### Supported Formats

- JPEG (.jpg, .jpeg)
- PNG (.png)
- GIF (.gif)
- BMP (.bmp)
- TIFF (.tiff)
- WebP (.webp)

## 🧠 How It Works

### Indexing Process

1. **Directory Walking**: Recursively scans for supported image files
2. **Content Hashing**: Uses `oshash-rs` for efficient media file hashing
3. **Duplicate Detection**: Skips already-indexed images based on path and hash
4. **Embedding Generation**: Creates 512-dimensional CLIP embeddings via `clipper`
5. **Normalization**: Vector normalization for efficient cosine similarity
6. **Storage**: Saves embeddings as binary data in SQLite

### Search Process

1. **Query Embedding**: Generates CLIP embedding for search text
2. **Similarity Computation**: Calculates cosine similarity (dot product of normalized vectors)
3. **Ranking**: Returns top-N results sorted by similarity score
4. **Display**: Shows results with similarity scores

### Performance Features

- **Incremental Indexing**: Only processes new or changed images
- **Efficient Hashing**: Media-optimized hash function for change detection
- **Normalized Vectors**: Fast cosine similarity via dot product
- **SQLite Optimization**: Indexed database for fast lookups

## 📊 Example Output

```bash
$ imgfind search "outdoor nature scene" --limit 3

Found 3 results for "outdoor nature scene":

  1. /Users/you/Pictures/vacation/mountain_hike.jpg     (similarity: 0.8234)
  2. /Users/you/Pictures/nature/forest_trail.jpg       (similarity: 0.7891)
  3. /Users/you/Pictures/camping/lake_sunrise.jpg      (similarity: 0.7456)
```

```bash
$ imgfind status

imgfind Database Status
======================
Database location: /Users/you/.imgfind/imgfind.db
Total indexed images: 1,247

Sample images:
  1. /Users/you/Pictures/family/birthday_2024.jpg
  2. /Users/you/Pictures/vacation/beach_sunset.jpg
  3. /Users/you/Pictures/pets/cat_sleeping.jpg
  4. /Users/you/Pictures/food/homemade_pizza.jpg
  5. /Users/you/Pictures/work/presentation.jpg
  ... and 1,242 more

Database size: 12.34 MB
```

## 🔧 Advanced Usage

### Environment Variables

- `RUST_LOG=info`: Enable detailed logging
- `RUST_LOG=debug`: Enable debug logging for troubleshooting

### Search Tips

- Use descriptive, natural language queries
- Try different phrasings if initial results aren't optimal
- Similarity scores range from -1 to 1 (higher is better)
- Use `--limit` to control number of results

### Performance Tips

- Initial indexing may take time for large collections
- Re-indexing is fast due to content-based change detection
- Consider periodic cleanup with `imgfind clean`

## 🧰 Development

### Dependencies

- Rust 1.70+
- CLIP model (downloaded automatically on first use)
- SQLite (bundled)

### Building

```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release

# Run tests
cargo test
```

### Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run tests and ensure they pass
5. Submit a pull request

## 📄 License

MIT License - see LICENSE file for details

## 🙏 Acknowledgments

- [CLIP](https://openai.com/blog/clip/) by OpenAI for the underlying model
- [oshash-rs](https://github.com/stevenwcarter/oshash-rs) for efficient media hashing
- The Rust community for excellent ecosystem tools

---

© 2025 `imgfind` Contributors
