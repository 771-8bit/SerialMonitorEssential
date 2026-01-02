// Plotter module - Real-time data visualization
//
// Provides parsing, storage, and visualization of serial data as graphs.

mod aggregator;
mod data_store;
mod parser;
mod thread;

pub use aggregator::PlotterAggregator;
pub use data_store::{AggregationMode, PlotterDataRequest, PlotterRangedPayload};
pub use parser::PlotterParser;
pub use thread::PlotterThread;
