import clsx from 'clsx';
import { SearchResultItem } from '../page/searchUrl';

interface LightboxViewerProps {
  item: SearchResultItem;
  handleClick: (e: React.MouseEvent, item: SearchResultItem) => void;
}

export const LightboxViewer = (props: LightboxViewerProps) => {
  const { item, handleClick } = props;

  const classes = clsx(
    'w-full h-auto',
    'cursor-pointer',
    'hover:opacity-80',
    'transition-all ease-in-out duration-100',
    'rounded-lg',
    'shadow-md hover:shadow-white',
    'hover:scale-105',
  );

  return (
    <div className={`flex justify-center items-center`}>
      <div className="relative group">
        <img
          className={classes}
          src={`/api/v1/search/thumb:300/${item.path}`}
          loading="lazy"
          alt={item.path}
          onClick={(e) => handleClick(e, item)}
        />
        {item.fileSize != null && (
          <span className="absolute bottom-0.5 right-2 opacity-0 group-hover:opacity-80 transition-opacity">
            {Math.round(item.fileSize / 1024)} KB
          </span>
        )}
      </div>
    </div>
  );
};

export default LightboxViewer;
