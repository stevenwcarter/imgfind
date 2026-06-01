import clsx from 'clsx';
import { ImageFromServer } from 'page/Images';

interface LightboxViewerProps {
  image: ImageFromServer; // [filename, filesize, base64]
  mainSrc?: string;
  mainSrcThumbnail?: string;
  handleClick: (e: any, image: ImageFromServer) => void;
}

export const LightboxViewer = (props: LightboxViewerProps) => {
  const { image, handleClick } = props;

  const classes = clsx(
    'w-full h-auto',
    'cursor-pointer',
    'hover:opacity-80',
    'transition-all ease-in-out duration-100',
    'rounded-lg',
    'shadow-md hover:shadow-white',
    'hover:scale-105',
  );

  const imageSrc = image[2]
    ? `data:image/jpg;base64,${image[2]}`
    : `/api/v1/search/thumb:300/${image[0]}`;

  // TODO - update this to dynamically set size
  return (
    <div className={`flex justify-center items-center`}>
      <div className="relative group">
        <img
          className={classes}
          src={imageSrc}
          alt={image[0]}
          onClick={(e) => handleClick(e, image)}
        />
        <span className="absolute bottom-0.5 right-2 opacity-0 group-hover:opacity-80 transition-opacity">
          {image[1]}
        </span>
      </div>
    </div>
  );
};

export default LightboxViewer;
