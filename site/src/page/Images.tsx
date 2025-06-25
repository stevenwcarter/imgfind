import clsx from 'clsx';
import { useEffect, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import Lightbox from 'yet-another-react-lightbox';
import Zoom from 'yet-another-react-lightbox/plugins/zoom';
import 'yet-another-react-lightbox/styles.css';

const LightboxViewer = (props) => {
  const { image } = props;
  const [isOpen, setIsOpen] = useState(false);

  const zoomRef = useRef(null);

  const handleClick = (e: any) => {
    if (e.ctrlKey) {
      window.open(props.mainSrc, '_blank');
    } else {
      setIsOpen(true);
    }
  };

  const classes = clsx(
    'max-w-[300px] max-h-[300px]',
    'object-scale-down',
    'cursor-pointer',
    'hover:opacity-80',
    'transition-all ease-in-out duration-100',
    'rounded-lg',
    'shadow-md hover:shadow-white',
    'hover:scale-105',
  );

  // TODO - update this to dynamically set size
  return (
    <div className={`flex justify-center items-center`}>
      <div className="relative group">
        <img
          className={classes}
          src={`/api/v1/search/thumb:300/${image[0]}`}
          alt={`${image[0]}`}
          onClick={() => handleClick(image[0])}
        />
        <span className="absolute bottom-0.5 right-2 opacity-0 group-hover:opacity-80 transition-opacity">
          {image[1]}
        </span>
      </div>
      <Lightbox
        plugins={[Zoom]}
        zoom={{ ref: zoomRef, scrollToZoom: true, maxZoomPixelRatio: 2 }}
        open={isOpen}
        slides={[{ src: `/api/v1/search/file/${image[0]}` }]}
        // mainSrcThumbnail={props.mainSrcThumbnail}
        close={() => setIsOpen(false)}
      />
    </div>
  );
};

export const Images = () => {
  const [search, setSearch] = useSearchParams();
  const [query, setQuery] = useState(search.get('query') || '');
  const [images, setImages] = useState<string[]>([]);
  const [selectedImage, setSelectedImage] = useState<string | null>(null);

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

  useEffect(() => {
    const s = search.get('query');

    if (s !== null && s !== '') {
      getImages(s);
    }
  }, [search]);

  console.log('IMAGES: ', images);

  return (
    <div>
      <h1 className="mb-4">imgfind</h1>
      <input
        type="text"
        value={query}
        className="border border-gray-300 rounded p-2 mb-4 bg-gray-600 text-white"
        onChange={(event) => setQuery(event.currentTarget.value)}
        onKeyUp={handleKeyUp}
        placeholder="Search images..."
      />
      <div className="flex flex-wrap gap-4 p-4">
        {images &&
          images.length > 0 &&
          images.map((image) => <LightboxViewer key={image[0]} image={image} />)}
      </div>
    </div>
  );
};

export default Images;
