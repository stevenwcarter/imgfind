import { Outlet, Link, useLocation } from 'react-router-dom';

export const PageTemplate = () => {
  const location = useLocation();

  return (
    <div className="h-full flex flex-col text-white">
      {/* Navigation */}
      <nav className="bg-gray-800 p-4 border-b border-gray-700">
        <div className="flex space-x-6">
          <Link
            to="/"
            className={`px-3 py-2 rounded transition-colors ${
              location.pathname === '/'
                ? 'bg-blue-600 text-white'
                : 'text-gray-300 hover:text-white hover:bg-gray-700'
            }`}
          >
            🔍 Search
          </Link>
          <Link
            to="/map"
            className={`px-3 py-2 rounded transition-colors ${
              location.pathname === '/map'
                ? 'bg-blue-600 text-white'
                : 'text-gray-300 hover:text-white hover:bg-gray-700'
            }`}
          >
            🗺️ Map
          </Link>
        </div>
      </nav>

      {/* Content */}
      <div className="flex flex-col p-4 md:p-10 flex-1 overflow-auto">
        <Outlet />
      </div>
    </div>
  );
};

export default PageTemplate;
