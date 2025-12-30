// Plotter module - Real-time data visualization
//
// Provides parsing, storage, and visualization of serial data as graphs.

mod data_store;
mod parser;
mod thread;

pub use data_store::{PlotterDataRequest, PlotterDataStore, PlotterRangedPayload};
pub use parser::PlotterParser;
pub use thread::PlotterThread;
