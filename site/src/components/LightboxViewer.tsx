import clsx from 'clsx';
import { ImageFromServer } from 'page/Images';
import { useRef, useState } from 'react';
import Lightbox from 'yet-another-react-lightbox';
import Zoom from 'yet-another-react-lightbox/plugins/zoom';
import Download from 'yet-another-react-lightbox/plugins/download';
import 'yet-another-react-lightbox/styles.css';

interface LightboxViewerProps {
  image: ImageFromServer; // [filename, filesize, base64]
  mainSrc?: string;
  mainSrcThumbnail?: string;
}

export const LightboxViewer = (props: LightboxViewerProps) => {
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

  const imageSrc = image[2]
    ? `data:image/jpg;base64,${image[2]}`
    : `/api/v1/search/file/${image[0]}`;

  // TODO - update this to dynamically set size
  return (
    <div className={`flex justify-center items-center`}>
      <div className="relative group">
        <img
          className={classes}
          src={imageSrc}
          alt={image[0]}
          onClick={() => handleClick(image[0])}
        />
        <span className="absolute bottom-0.5 right-2 opacity-0 group-hover:opacity-80 transition-opacity">
          {image[1]}
        </span>
      </div>
      <Lightbox
        plugins={[Zoom, Download]}
        zoom={{ ref: zoomRef, scrollToZoom: true, maxZoomPixelRatio: 2 }}
        open={isOpen}
        slides={[{ src: `/api/v1/search/file/${image[0]}` }]}
        // mainSrcThumbnail={props.mainSrcThumbnail}
        close={() => setIsOpen(false)}
      />
    </div>
  );
};

export default LightboxViewer;
