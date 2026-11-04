// Plotter module - Real-time data visualization
//
// Provides parsing, storage, and visualization of serial data as graphs.

mod aggregator;
mod data_store;
mod parser;
mod spill;
mod thread;

pub use aggregator::{PlotterAggregator, PlotterVersionInfo};
pub use data_store::{AggregationMode, PlotterChartPayload, PlotterConfig, PlotterDataRequest};
pub use parser::PlotterParser;
pub use thread::PlotterThread;
