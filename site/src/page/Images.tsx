import { useEffect, useState } from 'react';
import { useSearchParams } from 'react-router-dom';

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

  const handleImageClick = (imagePath: string) => {
    setSelectedImage(imagePath);
  };

  const closeModal = () => {
    setSelectedImage(null);
  };

  const handleModalClick = (event: React.MouseEvent<HTMLDivElement>) => {
    // Close modal if clicking on the backdrop (not the image)
    if (event.target === event.currentTarget) {
      closeModal();
    }
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
            <img
              key={image[0]}
              className="w-[250px] object-scale-down cursor-pointer hover:opacity-80 transition-opacity rounded-lg shadow-md"
              src={`/api/v1/search/file/${image[0]}`}
              alt={`${image[0]}`}
              onClick={() => handleImageClick(image[0])}
            />
          ))}
      </div>
      <p>end images</p>

      {/* Modal */}
      {selectedImage && (
        <div
          className="fixed inset-0 bg-black bg-opacity-75 flex items-center justify-center z-50"
          onClick={handleModalClick}
        >
          <div className="relative max-w-[90vw] max-h-[90vh] flex items-center justify-center">
            {/* Close button */}
            <button
              className="absolute top-4 right-4 text-white text-2xl font-bold bg-black bg-opacity-50 rounded-full w-8 h-8 flex items-center justify-center hover:bg-opacity-75 transition-colors z-10"
              onClick={closeModal}
              aria-label="Close modal"
            >
              ×
            </button>

            {/* Modal image */}
            <img
              className="max-w-full max-h-full object-scale-down rounded-lg"
              src={`/api/v1/search/file/${selectedImage}`}
              alt={selectedImage}
            />
          </div>
        </div>
      )}
    </div>
  );
};

export default Images;
