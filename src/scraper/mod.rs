pub mod engine;
pub mod parser;

pub use engine::{CreativeAwardsScraperEngine, ScrapeResult, ScraperEngine};
pub use parser::{parse_items_from_html, ScrapedItem};
