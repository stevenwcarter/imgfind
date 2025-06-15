import React, { Suspense } from 'react';
import { ApolloClient, ApolloProvider, InMemoryCache } from '@apollo/client';
import { ToastContainer } from 'react-toastify';
import { RouterProvider, createBrowserRouter } from 'react-router-dom';
import 'react-toastify/dist/ReactToastify.css';

const PageTemplate = React.lazy(() => import('page/PageTemplate'));
const Images = React.lazy(() => import('page/Images'));

const apolloClient = new ApolloClient({
  cache: new InMemoryCache({ addTypename: false }),
  uri: '/graphql',
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
