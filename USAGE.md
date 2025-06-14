# imgfind - Usage Guide

## Installation

Build the project from source:

```bash
cargo build --release
```

The binary will be available at `target/release/imgfind`.

## Usage

### 1. Index Images

Index all images in the current directory:
```bash
imgfind index
```

Index images in a specific directory recursively:
```bash
imgfind index --dir /path/to/images --recursive
```

### 2. Search for Images

Search for images matching a natural language query:
```bash
imgfind search "a cat sitting on a chair"
```

Limit the number of results:
```bash
imgfind search "sunset over mountains" --limit 5
```

### 3. Clean Database

Remove entries for images that no longer exist:
```bash
imgfind clean
```

## Database Location

The database is stored at `~/.imgfind/imgfind.db` by default. The tool will also search up the directory tree for an existing database.

## Supported Image Formats

- JPEG (.jpg, .jpeg)
- PNG (.png)
- GIF (.gif)
- BMP (.bmp)
- TIFF (.tiff)
- WebP (.webp)

## Example Workflow

1. Index your photo collection:
   ```bash
   imgfind index --dir ~/Pictures --recursive
   ```

2. Search for specific images:
   ```bash
   imgfind search "beach vacation"
   imgfind search "family dinner"
   imgfind search "landscape photography"
   ```

3. Periodically clean up the database:
   ```bash
   imgfind clean
   ```

## Performance Notes

- Initial indexing may take time depending on the number of images
- The CLIP model will be downloaded automatically on first use
- Embeddings are normalized and stored for efficient similarity search
- The database uses SQLite for reliable storage and fast querying
