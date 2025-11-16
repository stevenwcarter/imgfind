import React, { useEffect, useState, useRef, useCallback } from 'react';
import { gql } from '@apollo/client';
import { MapContainer, TileLayer, Marker, useMapEvents } from 'react-leaflet';
import { LatLngBounds, Icon, Map as LeafletMap } from 'leaflet';
import { useKeyPress, useBodyScrollLock } from '../hooks';
import Lightbox from 'yet-another-react-lightbox';
import Download from 'yet-another-react-lightbox/plugins/download';
import Thumbnails from 'yet-another-react-lightbox/plugins/thumbnails';
import Zoom from 'yet-another-react-lightbox/plugins/zoom';
import 'leaflet/dist/leaflet.css';
import 'yet-another-react-lightbox/styles.css';
import 'yet-another-react-lightbox/plugins/thumbnails.css';

// GraphQL query for images within bounds
const imagesByBounds = gql`
  query ImagesByBounds($north: Float!, $south: Float!, $east: Float!, $west: Float!) {
    imagesByBounds(north: $north, south: $south, east: $west, west: $east) {
      images {
        path
        latitude
        longitude
        thumbnailBase64
        width
        height
        datetimeTaken
      }
      originalCount
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

// Map events handler component
const MapEventsHandler: React.FC<{ onBoundsChange: () => void }> = ({ onBoundsChange }) => {
  useMapEvents({
    moveend: onBoundsChange,
    zoomend: onBoundsChange,
  });
  return null;
};

// Custom marker icon for images
const createImageIcon = (map: LeafletMap | null, thumbnailBase64?: string | null) => {
  let size = 40;
  if (map && map.getZoom() > 15) {
    size = 80;
  }
  if (map && map.getZoom() > 20) {
    size = 160;
  }
  if (thumbnailBase64) {
    return new Icon({
      iconUrl: `data:image/jpeg;base64,${thumbnailBase64}`,
      iconSize: [size, size],
      iconAnchor: [20, 20],
      popupAnchor: [0, -20],
      className: 'rounded-full border-2 border-white shadow-md',
    });
  }

  // Default marker icon
  return new Icon({
    iconUrl:
      'https://raw.githubusercontent.com/pointhi/leaflet-color-markers/master/img/marker-icon-2x-blue.png',
    shadowUrl: 'https://cdnjs.cloudflare.com/ajax/libs/leaflet/0.7.7/images/marker-shadow.png',
    iconSize: [25, 41],
    iconAnchor: [12, 41],
    popupAnchor: [1, -34],
    shadowSize: [41, 41],
  });
};

export const MapView: React.FC = () => {
  const [images, setImages] = useState<ImageLocation[]>([]);
  const [originalCount, setOriginalCount] = useState(0);
  const [lightboxOpen, setLightboxOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [bounds, setBounds] = useState<LatLngBounds | null>(null);
  const mapRef = useRef<LeafletMap | null>(null);

  // Fetch geotagged images using GraphQL
  const fetchGeotaggedImages = useCallback(async () => {
    setLoading(true);
    setError(null);

    const queryBounds = bounds || {
      getNorth: () => 90,
      getSouth: () => -90,
      getEast: () => 180,
      getWest: () => -180,
    };

    try {
      const response = await fetch('/graphql', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          query: imagesByBounds.loc?.source.body,
          variables: {
            north: queryBounds.getNorth(),
            south: queryBounds.getSouth(),
            east: queryBounds.getEast(),
            west: queryBounds.getWest(),
          },
        }),
      });

      if (!response.ok) {
        throw new Error('Network response was not ok');
      }

      const data = await response.json();

      if (data.errors) {
        throw new Error(data.errors[0].message);
      }

      setImages(data.data?.imagesByBounds?.images || []);
      setOriginalCount(data.data?.imagesByBounds?.originalCount || 0);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Unknown error occurred');
      console.error('Error fetching geotagged images:', err);
    } finally {
      setLoading(false);
    }
  }, [bounds]);
  // Default center (world view)
  const defaultCenter: [number, number] = [39.8283, -98.5795]; // Center of USA
  const defaultZoom = 4;

  // Handle escape key to close lightbox
  useKeyPress('Escape', () => setLightboxOpen(false), lightboxOpen);
  useBodyScrollLock(lightboxOpen);

  // Handle map bounds change
  const handleBoundsChange = () => {
    const map = mapRef.current;
    if (map) {
      const mapBounds = map.getBounds();
      setBounds(mapBounds);
    }
  };

  // Fetch images when bounds change
  useEffect(() => {
    if (bounds) {
      fetchGeotaggedImages();
    }
  }, [bounds, fetchGeotaggedImages]);

  // Load images on component mount
  useEffect(() => {
    fetchGeotaggedImages();
  }, [fetchGeotaggedImages]);

  // Handle marker click to open lightbox
  const handleMarkerClick = (image: ImageLocation) => {
    const imageIndex = images.findIndex((img) => img.path === image.path);
    setActiveIndex(imageIndex);
    setLightboxOpen(true);
  };

  // Close lightbox
  const handleCloseLightbox = () => {
    setLightboxOpen(false);
  };

  return (
    <div className="h-screen w-full">
      <div className="mb-4 p-4">
        <h1>Image Map</h1>
        <p className="text-sm text-gray-300">
          Navigate the map to find geotagged images. Click on image markers to view full size.
        </p>
        {loading && <p className="text-yellow-400">Loading images...</p>}
        {error && <p className="text-red-400">Error loading images: {error}</p>}
        {bounds && (
          <p className="text-blue-400 text-xs">
            Bounds: {bounds.getNorth().toFixed(2)}°N, {bounds.getSouth().toFixed(2)}°S,{' '}
            {bounds.getEast().toFixed(2)}°E, {bounds.getWest().toFixed(2)}°W
          </p>
        )}
        {images.length > 0 && (
          <>
            <p className="text-green-400">
              Showing {images.length} geotagged images in current view
            </p>
            <p className="text-green-400">Found {originalCount} geotagged images in current view</p>
          </>
        )}
        {mapRef.current && <p className="text-blue-400">Zoom: {mapRef.current?.getZoom()}</p>}
      </div>

      <div className="h-[65vh] border rounded-lg overflow-hidden">
        <MapContainer
          center={defaultCenter}
          zoom={defaultZoom}
          maxZoom={26}
          style={{ height: '100%', width: '100%' }}
          ref={mapRef}
          whenReady={() => {
            // Set initial bounds when map is ready
            handleBoundsChange();
          }}
        >
          {/* Map event handler */}
          <MapEventsHandler onBoundsChange={handleBoundsChange} />

          {/* OpenStreetMap tiles */}
          <TileLayer
            attribution='&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors'
            url="https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png"
          />

          {/* Image markers */}
          {images.map((image, index) => (
            <Marker
              key={`${image.path}-${index}`}
              position={[image.latitude, image.longitude]}
              icon={createImageIcon(mapRef.current, image.thumbnailBase64)}
              eventHandlers={{
                click: () => handleMarkerClick(image),
              }}
            >
              {/* <Popup> */}
              {/*   <div className="text-center"> */}
              {/*     {image.thumbnailBase64 && ( */}
              {/*       <img */}
              {/*         src={`data:image/jpeg;base64,${image.thumbnailBase64}`} */}
              {/*         alt={image.path} */}
              {/*         className="w-32 h-32 object-cover rounded mb-2 cursor-pointer" */}
              {/*         onClick={() => handleMarkerClick(image)} */}
              {/*       /> */}
              {/*     )} */}
              {/*     <div className="text-sm"> */}
              {/*       <p className="font-semibold">{image.path.split('/').pop()}</p> */}
              {/*       {image.datetimeTaken && <p className="text-gray-600">{image.datetimeTaken}</p>} */}
              {/*       {image.width && image.height && ( */}
              {/*         <p className="text-gray-600"> */}
              {/*           {image.width} × {image.height} */}
              {/*         </p> */}
              {/*       )} */}
              {/*       <button */}
              {/*         onClick={() => handleMarkerClick(image)} */}
              {/*         className="mt-2 px-3 py-1 bg-blue-500 text-white rounded text-xs hover:bg-blue-600" */}
              {/*       > */}
              {/*         View Full Size */}
              {/*       </button> */}
              {/*     </div> */}
              {/*   </div> */}
              {/* </Popup> */}
            </Marker>
          ))}
        </MapContainer>
      </div>

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
