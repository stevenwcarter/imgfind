import React, { useEffect, useState } from 'react';
import { gql } from '@apollo/client';
import { useKeyPress, useBodyScrollLock } from '../hooks';
import Lightbox from 'yet-another-react-lightbox';
import Download from 'yet-another-react-lightbox/plugins/download';
import Thumbnails from 'yet-another-react-lightbox/plugins/thumbnails';
import Zoom from 'yet-another-react-lightbox/plugins/zoom';
import 'yet-another-react-lightbox/styles.css';
import 'yet-another-react-lightbox/plugins/thumbnails.css';

// GraphQL query for images within bounds
const IMAGES_BY_BOUNDS = gql`
  query ImagesByBounds($north: Float!, $south: Float!, $east: Float!, $west: Float!) {
    images_by_bounds(north: $north, south: $south, east: $east, west: $west) {
      path
      latitude
      longitude
      thumbnailBase64
      width
      height
      datetimeTaken
    }
  }
`;

interface ImageLocation {
  path: string;
  latitude: number;
  longitude: number;
  thumbnailBase64?: string | null;
  width?: number | null;
  height?: number | null;
  datetimeTaken?: string | null;
}

export const MapView: React.FC = () => {
  const [images, setImages] = useState<ImageLocation[]>([]);
  const [lightboxOpen, setLightboxOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Bounds for demo - this would normally come from map interaction
  const [bounds] = useState({
    north: 90,
    south: -90,
    east: 180,
    west: -180,
  });

  // Handle escape key to close lightbox
  useKeyPress('Escape', () => setLightboxOpen(false), lightboxOpen);
  useBodyScrollLock(lightboxOpen);

  // Fetch geotagged images using GraphQL
  const fetchGeotaggedImages = async () => {
    setLoading(true);
    setError(null);

    try {
      const response = await fetch('/graphql', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          query: IMAGES_BY_BOUNDS.loc?.source.body,
          variables: bounds,
        }),
      });

      if (!response.ok) {
        throw new Error('Network response was not ok');
      }

      const data = await response.json();

      if (data.errors) {
        throw new Error(data.errors[0].message);
      }

      setImages(data.data?.images_by_bounds || []);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Unknown error occurred');
      console.error('Error fetching geotagged images:', err);
    } finally {
      setLoading(false);
    }
  };

  // Load images on component mount
  useEffect(() => {
    fetchGeotaggedImages();
  }, []);

  // Handle image click to open lightbox
  const handleImageClick = (image: ImageLocation) => {
    const imageIndex = images.findIndex((img) => img.path === image.path);
    setActiveIndex(imageIndex);
    setLightboxOpen(true);
  };

  // Close lightbox
  const handleCloseLightbox = () => {
    setLightboxOpen(false);
  };

  return (
    <div className="h-full w-full p-4">
      <div className="mb-6">
        <h1>Geotagged Images</h1>
        <p className="text-sm text-gray-300 mb-4">
          View all images with GPS coordinates. Interactive map coming soon!
        </p>

        <button
          onClick={fetchGeotaggedImages}
          disabled={loading}
          className="px-4 py-2 bg-blue-500 text-white rounded hover:bg-blue-600 disabled:bg-gray-400"
        >
          {loading ? 'Loading...' : 'Refresh Images'}
        </button>

        {loading && <p className="text-yellow-400 mt-2">Loading geotagged images...</p>}
        {error && <p className="text-red-400 mt-2">Error: {error}</p>}
        {images.length > 0 && (
          <p className="text-green-400 mt-2">Found {images.length} geotagged images</p>
        )}
      </div>

      {/* Grid of geotagged images */}
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-4 pb-8">
        {images.map((image, index) => (
          <div
            key={`${image.path}-${index}`}
            className="bg-gray-800 rounded-lg p-4 hover:bg-gray-700 transition-colors cursor-pointer"
            onClick={() => handleImageClick(image)}
          >
            {/* Thumbnail */}
            {image.thumbnailBase64 ? (
              <img
                src={`data:image/jpeg;base64,${image.thumbnailBase64}`}
                alt={image.path}
                className="w-full h-48 object-cover rounded mb-3"
              />
            ) : (
              <div className="w-full h-48 bg-gray-600 rounded mb-3 flex items-center justify-center">
                <span className="text-gray-400">No thumbnail</span>
              </div>
            )}

            {/* Image details */}
            <div className="text-sm space-y-1">
              <p className="font-semibold text-white truncate" title={image.path}>
                {image.path.split('/').pop()}
              </p>

              <p className="text-gray-300">
                📍 {image.latitude.toFixed(4)}, {image.longitude.toFixed(4)}
              </p>

              {image.datetimeTaken && <p className="text-gray-400">📅 {image.datetimeTaken}</p>}

              {image.width && image.height && (
                <p className="text-gray-400">
                  📐 {image.width} × {image.height}
                </p>
              )}
            </div>
          </div>
        ))}
      </div>

      {/* Empty state */}
      {!loading && images.length === 0 && !error && (
        <div className="text-center py-12">
          <p className="text-gray-400 text-lg mb-4">No geotagged images found</p>
          <p className="text-gray-500">Index some images with GPS coordinates to see them here.</p>
        </div>
      )}

      {/* Lightbox for viewing full-size images */}
      {lightboxOpen && images.length > 0 && (
        <Lightbox
          plugins={[Download, Thumbnails, Zoom]}
          open={lightboxOpen}
          close={handleCloseLightbox}
          index={activeIndex}
          slides={images.map((img) => ({
            src: `/api/v1/search/file/${img.path}`,
            thumbnail: img.thumbnailBase64
              ? `data:image/jpeg;base64,${img.thumbnailBase64}`
              : `/api/v1/search/thumb:300/${img.path}`,
          }))}
          carousel={{ preload: 3 }}
          zoom={{ scrollToZoom: true, maxZoomPixelRatio: 2 }}
          thumbnails={{ showToggle: true }}
        />
      )}
    </div>
  );
};

export default MapView;
