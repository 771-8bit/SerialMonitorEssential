import React from 'react';
import ReactDOM from 'react-dom/client';
import PlotterWindow from './components/plotter/PlotterWindow';
import './App.css';

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <PlotterWindow />
  </React.StrictMode>
);
