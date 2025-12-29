// Plotter module - Real-time data visualization
//
// Provides parsing, storage, and visualization of serial data as graphs.

#[allow(dead_code)] // Reserved for future use - plotter feature in development
mod data_store;
#[allow(dead_code)]
mod parser;
#[allow(dead_code)]
mod thread;

pub use data_store::{PlotterDataPayload, PlotterDataStore};
pub use parser::PlotterParser;
pub use thread::PlotterThread;
