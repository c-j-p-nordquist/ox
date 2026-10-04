//! Renders a [`Brief`] into a self-contained, phone-first HTML page (the data is embedded as JSON
//! and drawn by a small inline script).

use anyhow::Result;

use crate::brief::Brief;

const TEMPLATE: &str = include_str!("brief_template.html");

pub fn render(b: &Brief) -> Result<String> {
    // `</` inside an inline script would end it early.
    let json = serde_json::to_string(b)?.replace("</", "<\\/");
    Ok(TEMPLATE.replace("/*BRIEF_JSON*/", &json))
}
