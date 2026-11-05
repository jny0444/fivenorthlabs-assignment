use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Deserialize;

use crate::models::Message;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub id: String,
    pub reply: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<Line>,
    pub score: Score,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Score {
    pub wrong_bookings: usize,
    pub unnecessary_questions: usize,
    pub handled_correctly: usize,
    pub total: usize,
    pub misses: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MockRow {
    id: String,
    intent: String,
    date: Option<String>,
    time: Option<String>,
    doctor_id: Option<String>,
    #[serde(default)]
    any_doctor: bool,
    #[serde(default)]
    earliest: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Expected {
    id: String,
    action: String,
    #[serde(default)]
    with: Option<String>,
    #[serde(default)]
    doctor_id: Option<String>,
    #[serde(default)]
    doctor_name: Option<String>,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default = "survives_by_default")]
    survives: bool,
    clear_enough: bool,
}

fn survives_by_default() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct StoredBooking {
    doctor_id: String,
    start: String,
}

pub async fn play(
    base: &str,
    messages_path: &Path,
    mock_path: &Path,
    expected_path: &Path,
) -> Result<Report, Box<dyn std::error::Error + Send + Sync>> {
    let messages: Vec<Message> = serde_json::from_str(&std::fs::read_to_string(messages_path)?)?;
    let mocks: Vec<MockRow> = serde_json::from_str(&std::fs::read_to_string(mock_path)?)?;
    let mocks = mocks
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect::<HashMap<_, _>>();
    let client = reqwest::Client::new();
    let mut done = HashSet::new();
    let mut lines = Vec::new();

    for message in &messages {
        if !done.insert(message.id.clone()) {
            continue;
        }
        let mock = mocks
            .get(&message.id)
            .ok_or_else(|| format!("mock.json has no reading for {}", message.id))?;
        if let Some(other_id) = &message.sent_at_same_time_as {
            if let Some(other) = messages.iter().find(|item| &item.id == other_id) {
                if done.insert(other.id.clone()) {
                    let other_mock = mocks
                        .get(&other.id)
                        .ok_or_else(|| format!("mock.json has no reading for {}", other.id))?;
                    let (left, right) = tokio::join!(
                        post(&client, base, message, mock),
                        post(&client, base, other, other_mock),
                    );
                    let mut pair = vec![left?, right?];
                    pair.sort_by(|left, right| left.id.cmp(&right.id));
                    lines.extend(pair);
                    continue;
                }
            }
        }
        lines.push(post(&client, base, message, mock).await?);
    }

    let expected: Vec<Expected> = serde_json::from_str(&std::fs::read_to_string(expected_path)?)?;
    let bookings: Vec<StoredBooking> = client
        .get(format!("{base}/bookings"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let score = score(&lines, &bookings, &expected);
    Ok(Report { lines, score })
}

async fn post(
    client: &reqwest::Client,
    base: &str,
    message: &Message,
    mock: &MockRow,
) -> Result<Line, Box<dyn std::error::Error + Send + Sync>> {
    let response = client
        .post(format!("{base}/messages"))
        .json(&serde_json::json!({
            "from": message.from,
            "intent": mock.intent,
            "date": mock.date,
            "time": mock.time,
            "doctor_id": mock.doctor_id,
            "any_doctor": mock.any_doctor,
            "earliest": mock.earliest,
        }))
        .send()
        .await?;
    let status = response.status();
    let reply = response.text().await?;
    if !status.is_success() {
        return Err(format!("{} returned {status}: {reply}", message.id).into());
    }
    Ok(Line {
        id: message.id.clone(),
        reply,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Booked,
    Cancelled,
    Refused,
    Asked,
}

fn score(lines: &[Line], bookings: &[StoredBooking], expected: &[Expected]) -> Score {
    let replies = lines
        .iter()
        .map(|line| (line.id.clone(), line.reply.clone()))
        .collect::<HashMap<_, _>>();
    let wrong_bookings = wrong_rows(bookings, expected);
    let mut unnecessary_questions = 0;
    let mut handled_correctly = 0;
    let mut misses = Vec::new();

    for item in expected {
        if let Some(partner) = &item.with {
            if item.id.as_str() > partner.as_str() {
                continue;
            }
            let (correct, questions) = score_pair(item, partner, &replies);
            handled_correctly += correct;
            unnecessary_questions += questions;
            if correct < 2 {
                misses.push(format!(
                    "{} and {partner} were not one booking and one refusal for {}",
                    item.id,
                    item.when.as_deref().unwrap_or("the slot")
                ));
            }
            continue;
        }

        let reply = replies.get(&item.id).map(String::as_str).unwrap_or("");
        let seen = classify(reply);
        if seen == Seen::Asked && item.clear_enough && item.action != "asked" {
            unnecessary_questions += 1;
        }
        if message_ok(item, seen, reply) {
            handled_correctly += 1;
        } else {
            misses.push(format!(
                "{} expected {}, got {}",
                item.id,
                item.action,
                seen_name(seen)
            ));
        }
    }

    Score {
        wrong_bookings,
        unnecessary_questions,
        handled_correctly,
        total: expected.len(),
        misses,
    }
}

fn score_pair(item: &Expected, partner: &str, replies: &HashMap<String, String>) -> (usize, usize) {
    let left = replies.get(&item.id).map(String::as_str).unwrap_or("");
    let right = replies.get(partner).map(String::as_str).unwrap_or("");
    let mut questions = 0;
    if item.clear_enough && classify(left) == Seen::Asked {
        questions += 1;
    }
    if item.clear_enough && classify(right) == Seen::Asked {
        questions += 1;
    }
    let left_hit = classify(left) == Seen::Booked && details_match(item, left);
    let right_hit = classify(right) == Seen::Booked && details_match(item, right);
    let left_refused = classify(left) == Seen::Refused;
    let right_refused = classify(right) == Seen::Refused;
    let correct = match (left_hit, right_hit, left_refused, right_refused) {
        (true, false, _, true) | (false, true, true, _) => 2,
        _ => 0,
    };
    (correct, questions)
}

fn message_ok(item: &Expected, seen: Seen, reply: &str) -> bool {
    match item.action.as_str() {
        "booked" => seen == Seen::Booked && details_match(item, reply),
        "cancelled" => seen == Seen::Cancelled && details_match(item, reply),
        "refused" => seen == Seen::Refused,
        "asked" => seen == Seen::Asked,
        _ => false,
    }
}

fn details_match(item: &Expected, reply: &str) -> bool {
    item.doctor_name
        .as_ref()
        .is_some_and(|name| reply.contains(name))
        && item.when.as_ref().is_some_and(|when| reply.contains(when))
}

fn wrong_rows(bookings: &[StoredBooking], expected: &[Expected]) -> usize {
    let allowed = expected
        .iter()
        .filter(|item| item.survives)
        .filter_map(|item| Some((item.doctor_id.clone()?, item.start.clone()?)))
        .collect::<HashSet<_>>();
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    let mut wrong = 0;
    for booking in bookings {
        let key = (booking.doctor_id.clone(), booking.start.clone());
        *counts.entry(key.clone()).or_insert(0) += 1;
        if !allowed.contains(&key) {
            wrong += 1;
        }
    }
    for count in counts.values() {
        if *count > 1 {
            wrong += count - 1;
        }
    }
    wrong
}

fn classify(reply: &str) -> Seen {
    let reply = reply.trim();
    if reply.starts_with("You're booked") {
        Seen::Booked
    } else if reply.starts_with("Cancelled") {
        Seen::Cancelled
    } else if reply.ends_with('?') {
        Seen::Asked
    } else {
        Seen::Refused
    }
}

fn seen_name(seen: Seen) -> &'static str {
    match seen {
        Seen::Booked => "booked",
        Seen::Cancelled => "cancelled",
        Seen::Refused => "refused",
        Seen::Asked => "asked",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, reply: &str) -> Line {
        Line {
            id: id.to_string(),
            reply: reply.to_string(),
        }
    }

    fn expect_booked(id: &str) -> Expected {
        Expected {
            id: id.to_string(),
            action: "booked".to_string(),
            with: None,
            doctor_id: Some("rao".to_string()),
            doctor_name: Some("Dr. Rao".to_string()),
            when: Some("Tue 17 Nov, 11 AM".to_string()),
            start: Some("2026-11-17T11:00:00+05:30".to_string()),
            survives: true,
            clear_enough: true,
        }
    }

    #[test]
    fn an_extra_row_and_a_question_are_counted() {
        let expected = vec![expect_booked("m01")];
        let lines = vec![line("m01", "What day and time would you like?")];
        let bookings = vec![StoredBooking {
            doctor_id: "rao".to_string(),
            start: "2026-11-17T09:00:00+05:30".to_string(),
        }];
        let score = score(&lines, &bookings, &expected);
        assert_eq!(score.unnecessary_questions, 1);
        assert_eq!(score.wrong_bookings, 1);
        assert_eq!(score.handled_correctly, 0);
    }

    #[test]
    fn a_double_booked_slot_counts_as_one_extra() {
        let mut first = expect_booked("m09");
        first.action = "either".to_string();
        first.with = Some("m10".to_string());
        first.when = Some("Wed 18 Nov, 12 PM".to_string());
        first.start = Some("2026-11-18T12:00:00+05:30".to_string());
        let mut second = first.clone();
        second.id = "m10".to_string();
        second.with = Some("m09".to_string());
        let reply = "You're booked with Dr. Rao on Wed 18 Nov, 12 PM.";
        let lines = vec![line("m09", reply), line("m10", reply)];
        let bookings = vec![
            StoredBooking {
                doctor_id: "rao".to_string(),
                start: "2026-11-18T12:00:00+05:30".to_string(),
            },
            StoredBooking {
                doctor_id: "rao".to_string(),
                start: "2026-11-18T12:00:00+05:30".to_string(),
            },
        ];
        let score = score(&lines, &bookings, &[first, second]);
        assert_eq!(score.wrong_bookings, 1);
        assert_eq!(score.handled_correctly, 0);
        assert_eq!(score.total, 2);
    }
}
