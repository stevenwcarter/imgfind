import { useEffect, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { FontAwesomeIcon } from '@fortawesome/react-fontawesome';
import { faTimesCircle } from '@fortawesome/free-solid-svg-icons';
import LightboxViewer from 'components/LightboxViewer';
import Lightbox from 'yet-another-react-lightbox';
import Download from 'yet-another-react-lightbox/plugins/download';
import Thumbnails from 'yet-another-react-lightbox/plugins/thumbnails';
import Zoom from 'yet-another-react-lightbox/plugins/zoom';
import 'yet-another-react-lightbox/styles.css';
import 'yet-another-react-lightbox/plugins/thumbnails.css';

export type ImageFromServer = [string, string, string | null]; // [filename, filesize, base64]

export const Images = () => {
  const [search, setSearch] = useSearchParams();
  const [query, setQuery] = useState(search.get('query') || '');
  const [images, setImages] = useState<ImageFromServer[]>([]);
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

  // Handle escape key to close modal
  useEffect(() => {
    const handleEscapeKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && selectedImage) {
        closeModal();
      }
    };

    if (selectedImage) {
      document.addEventListener('keydown', handleEscapeKey);
      // Prevent body scroll when modal is open
      document.body.style.overflow = 'hidden';
    } else {
      document.body.style.overflow = 'unset';
    }

    return () => {
      document.removeEventListener('keydown', handleEscapeKey);
      document.body.style.overflow = 'unset';
    };
  }, [selectedImage]);

  const getImages = async (q: string) => {
    try {
      const response = await fetch(`/api/v1/search/${encodeURIComponent(q)}`);
      if (!response.ok) {
        throw new Error('Network response was not ok');
      }
      const data = await response.json();
      console.log('Images fetched:', data);
      setImages(data || []);
    } catch (error) {
      console.error('Error fetching images:', error);
    }
  };

  const handleClick = (e: any, image: ImageFromServer) => {
    if (e.ctrlKey) {
      const mainSrc = `/api/v1/search/file/${image[0]}`;
      window.open(mainSrc, '_blank');
    } else {
      setActiveIndex(images.findIndex((img) => img[0] === image[0]));
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
    }
  }, [search]);

  return (
    <div>
      <h1 className="mb-4">imgfind</h1>
      <div className="relative max-w-[200px] items-center">
        <input
          type="text"
          value={query}
          className="w-full border border-gray-300 rounded p-2 mb-4 bg-gray-600 text-white"
          onChange={(event) => setQuery(event.currentTarget.value)}
          onKeyUp={handleKeyUp}
          placeholder="Search images..."
        />
        {query !== '' && (
          <FontAwesomeIcon
            icon={faTimesCircle}
            className="absolute right-2 top-3 cursor-pointer hover:scale-125"
            onClick={handleClear}
          />
        )}
      </div>

      <div className="flex flex-wrap gap-4 p-4">
        {images &&
          images.length > 0 &&
          images.map((image) => (
            <LightboxViewer key={image[0]} image={image} handleClick={handleClick} />
          ))}
      </div>
      {images && images.length > 0 && (
        <Lightbox
          plugins={[Download, Thumbnails, Zoom]}
          thumbnails={{ ref: thumbnailsRef, showToggle: true }}
          carousel={{ preload: 3 }}
          zoom={{ ref: zoomRef, scrollToZoom: true, maxZoomPixelRatio: 2 }}
          open={isOpen}
          slides={images.map((img) => ({
            src: `/api/v1/search/file/${img[0]}`,
            thumbnail: `/api/v1/search/thumb:300/${img[0]}`,
          }))}
          close={handleClose}
          index={activeIndex}
        />
      )}
    </div>
  );
};

export default Images;
