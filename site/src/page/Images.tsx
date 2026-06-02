import { useEffect, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { FontAwesomeIcon } from '@fortawesome/react-fontawesome';
import { faTimesCircle } from '@fortawesome/free-solid-svg-icons';
import { useKeyPress, useBodyScrollLock } from '../hooks';
import { selectViewState } from './searchViewState';
import { buildSearchUrl, SearchResponse, SearchResultItem } from './searchUrl';
import LightboxViewer from 'components/LightboxViewer';
import Lightbox from 'yet-another-react-lightbox';
import Download from 'yet-another-react-lightbox/plugins/download';
import Thumbnails from 'yet-another-react-lightbox/plugins/thumbnails';
import Zoom from 'yet-another-react-lightbox/plugins/zoom';
import 'yet-another-react-lightbox/styles.css';
import 'yet-another-react-lightbox/plugins/thumbnails.css';

export const Images = () => {
  const [search, setSearch] = useSearchParams();
  const [query, setQuery] = useState(search.get('query') || '');
  const [images, setImages] = useState<SearchResultItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hasSearched, setHasSearched] = useState(false);
  const [hasMore, setHasMore] = useState(false);
  const [selectedImage, setSelectedImage] = useState<string | null>(null);
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const zoomRef = useRef(null);
  const thumbnailsRef = useRef(null);

  const handleKeyUp = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter') {
      setSearch({ query: event.currentTarget.value });
    }
  };

  const closeModal = () => {
    setSelectedImage(null);
  };

  // Handle escape key to close modal and manage body scroll
  useKeyPress('Escape', closeModal, !!selectedImage);
  useBodyScrollLock(!!selectedImage);

  const getImages = async (q: string, offset = 0) => {
    setLoading(true);
    setError(null);
    setHasSearched(true);
    try {
      const response = await fetch(buildSearchUrl(q, offset));
      if (!response.ok) {
        throw new Error(`Search failed (${response.status})`);
      }
      const data: SearchResponse = await response.json();
      setImages((prev) => (offset === 0 ? data.results : [...prev, ...data.results]));
      setHasMore(data.hasMore);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Search failed');
      if (offset === 0) setImages([]);
    } finally {
      setLoading(false);
    }
  };

  const handleClick = (e: React.MouseEvent, item: SearchResultItem) => {
    if (e.ctrlKey) {
      const mainSrc = `/api/v1/search/file/${item.path}`;
      window.open(mainSrc, '_blank');
    } else {
      setActiveIndex(images.findIndex((img) => img.path === item.path));
      setIsOpen(true);
    }
  };

  const handleClear = () => {
    setSearch({ query: '' });
    setQuery('');
  };

  const handleClose = () => {
    setIsOpen(false);
  };

  useEffect(() => {
    const s = search.get('query');

    if (s !== null && s !== '') {
      getImages(s);
    } else {
      setImages([]);
      setError(null);
      setHasSearched(false);
      setHasMore(false);
    }
  }, [search]);

  const viewState = selectViewState({
    hasSearched,
    loading,
    error,
    resultCount: images.length,
  });

  return (
    <div>
      <h1 className="mb-4">imgfind</h1>
      <div className="relative max-w-[200px] items-center">
        <input
          type="text"
          value={query}
          className="w-full border border-gray-300 rounded p-2 mb-4 bg-gray-600 text-white disabled:opacity-50"
          onChange={(event) => setQuery(event.currentTarget.value)}
          onKeyUp={handleKeyUp}
          placeholder="Search images..."
          disabled={loading}
        />
        {query !== '' && (
          <FontAwesomeIcon
            icon={faTimesCircle}
            className="absolute right-2 top-3 cursor-pointer hover:scale-125"
            onClick={handleClear}
          />
        )}
      </div>

      {viewState === 'idle' && (
        <p className="p-4 text-gray-400">Enter a search query to find images.</p>
      )}

      {viewState === 'loading' && (
        <p className="p-4 text-gray-300" role="status">
          Searching…
        </p>
      )}

      {viewState === 'error' && (
        <div
          role="alert"
          className="m-4 rounded border border-red-500 bg-red-900/40 p-3 text-red-200"
        >
          {error}
        </div>
      )}

      {viewState === 'empty' && (
        <p className="p-4 text-gray-400">No images found for &quot;{search.get('query')}&quot;.</p>
      )}

      {viewState === 'results' && (
        <div className="columns-2 gap-4 p-4 sm:columns-3 lg:columns-4">
          {images.map((item) => (
            <div key={item.path} className="mb-4 break-inside-avoid">
              <LightboxViewer item={item} handleClick={handleClick} />
            </div>
          ))}
        </div>
      )}

      {hasMore && !loading && (
        <div className="flex justify-center p-4">
          <button
            type="button"
            className="rounded bg-gray-700 px-4 py-2 text-white hover:bg-gray-600"
            onClick={() => getImages(query, images.length)}
          >
            Load more
          </button>
        </div>
      )}

      {images && images.length > 0 && (
        <Lightbox
          plugins={[Download, Thumbnails, Zoom]}
          thumbnails={{ ref: thumbnailsRef, showToggle: true }}
          carousel={{ preload: 3 }}
          zoom={{ ref: zoomRef, scrollToZoom: true, maxZoomPixelRatio: 2 }}
          open={isOpen}
          slides={images.map((item) => ({
            src: `/api/v1/search/file/${item.path}`,
            thumbnail: `/api/v1/search/thumb:300/${item.path}`,
          }))}
          close={handleClose}
          index={activeIndex}
        />
      )}
    </div>
  );
};

export default Images;
