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

  // TODO - update this to dynamically set size
  return (
    <div className={`flex justify-center`}>
      <img
        className="w-[250px] object-scale-down cursor-pointer hover:opacity-80 transition-opacity rounded-lg shadow-md"
        src={`/api/v1/search/thumb:300/${image[0]}`}
        alt={`${image[0]}`}
        onClick={() => handleClick(image[0])}
      />
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
      <h1>Images Page</h1>
      <input
        type="text"
        value={query}
        onChange={(event) => setQuery(event.currentTarget.value)}
        onKeyUp={handleKeyUp}
        placeholder="Search images..."
      />
      <p>This is the images page content.</p>
      <div className="flex flex-wrap gap-4 p-4">
        {images &&
          images.length > 0 &&
          images.map((image) => (
            <div className="flex flex-col justify-center" key={image[0]}>
              <LightboxViewer image={image} />
            </div>
          ))}
      </div>
      <p>end images</p>
    </div>
  );
};

export default Images;
