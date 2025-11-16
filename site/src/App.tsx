import React, { Suspense } from 'react';
import { ApolloClient, HttpLink, InMemoryCache } from '@apollo/client';
import { ApolloProvider } from '@apollo/client/react';
import { ToastContainer } from 'react-toastify';
import { RouterProvider, createBrowserRouter } from 'react-router-dom';
import 'react-toastify/dist/ReactToastify.css';

const PageTemplate = React.lazy(() => import('page/PageTemplate'));
const Images = React.lazy(() => import('page/Images'));
const MapView = React.lazy(() => import('page/MapView'));

const apolloClient = new ApolloClient({
  cache: new InMemoryCache({}),
  link: new HttpLink({ uri: '/graphql' }),
});

const router = createBrowserRouter([
  {
    path: '/',
    element: <PageTemplate />,
    children: [
      {
        index: true,
        element: <Images />,
      },
      {
        path: 'map',
        element: <MapView />,
      },
    ],
  },
]);

export const App = () => {
  return (
    <React.StrictMode>
      <ApolloProvider client={apolloClient}>
        <Suspense fallback={<div>Loading...</div>}>
          <ToastContainer />
          <RouterProvider router={router} />
        </Suspense>
      </ApolloProvider>
    </React.StrictMode>
  );
};

export default App;
