pub mod api;
pub mod decide;
pub mod models;
pub mod replay;
pub mod schedule;
pub mod store;

use std::path::Path;

use crate::models::ClinicConfig;

pub fn read_clinic(path: impl AsRef<Path>) -> Result<ClinicConfig, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}
