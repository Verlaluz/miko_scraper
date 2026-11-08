use crate::errors::AppError;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapedItem {
    pub title: String,
    pub details: Option<String>,
}

/// Generic HTML extractor that parses CSS selectors safely.
pub fn parse_items_from_html(
    html_content: &str,
    container_selector: &str,
    title_selector: &str,
) -> Result<Vec<ScrapedItem>, AppError> {
    let document = Html::parse_document(html_content);

    let container_sel = Selector::parse(container_selector)
        .map_err(|e| AppError::Scraper(format!("Invalid container CSS selector: {:?}", e)))?;
    let title_sel = Selector::parse(title_selector)
        .map_err(|e| AppError::Scraper(format!("Invalid title CSS selector: {:?}", e)))?;

    let mut results = Vec::new();

    for element in document.select(&container_sel) {
        if let Some(title_elem) = element.select(&title_sel).next() {
            let title = title_elem.text().collect::<Vec<_>>().join(" ").trim().to_string();
            if !title.is_empty() {
                results.push(ScrapedItem {
                    title,
                    details: None,
                });
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_html() {
        let html = r#"
            <html>
                <body>
                    <div class="card"><h2 class="title">Item 1</h2></div>
                    <div class="card"><h2 class="title">Item 2</h2></div>
                </body>
            </html>
        "#;
        let items = parse_items_from_html(html, ".card", ".title").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Item 1");
        assert_eq!(items[1].title, "Item 2");
    }
}
