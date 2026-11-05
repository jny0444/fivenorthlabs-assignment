use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClinicConfig {
    pub clinic: String,
    pub timezone: String,
    pub now: DateTime<FixedOffset>,
    pub slot_minutes: u32,
    pub closed_dates: Vec<NaiveDate>,
    pub doctors: Vec<Doctor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Doctor {
    pub id: String,
    pub name: String,
    pub hours: HashMap<String, Vec<TimeRange>>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct TimeRange {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

fn deserialize_time_range<'de, D>(deserializer: D) -> Result<TimeRange, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    let (start, end) = text
        .split_once('-')
        .ok_or_else(|| serde::de::Error::custom("expected HH:MM-HH:MM"))?;
    Ok(TimeRange {
        start: NaiveTime::parse_from_str(start, "%H:%M").map_err(serde::de::Error::custom)?,
        end: NaiveTime::parse_from_str(end, "%H:%M").map_err(serde::de::Error::custom)?,
    })
}

impl<'de> Deserialize<'de> for TimeRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserialize_time_range(deserializer)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at_same_time_as: Option<String>,
}
