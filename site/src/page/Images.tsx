import { useEffect, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { FontAwesomeIcon } from '@fortawesome/react-fontawesome';
import { faTimesCircle } from '@fortawesome/free-solid-svg-icons';
import LightboxViewer from 'components/LightboxViewer';

export type ImageFromServer = [string, string, string | null]; // [filename, filesize, base64]

export const Images = () => {
  const [search, setSearch] = useSearchParams();
  const [query, setQuery] = useState(search.get('query') || '');
  const [images, setImages] = useState<ImageFromServer[]>([]);
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
            onClick={() => setQuery('')}
          />
        )}
      </div>

      <div className="flex flex-wrap gap-4 p-4">
        {images &&
          images.length > 0 &&
          images.map((image) => <LightboxViewer key={image[0]} image={image} />)}
      </div>
    </div>
  );
};

export default Images;
