use std::collections::HashSet;

use chrono::{
    DateTime, Datelike, FixedOffset, NaiveDate, NaiveTime, TimeDelta, TimeZone, Timelike,
};

use crate::models::{ClinicConfig, Doctor, TimeRange};

/// One bookable appointment. `end` is exclusive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Slot {
    pub doctor_id: String,
    pub doctor_name: String,
    pub start: DateTime<FixedOffset>,
    pub end: DateTime<FixedOffset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    pub closed: bool,
    pub slots: Vec<Slot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    UnknownDoctor,
    Closed { date: NaiveDate },
    Past,
    OutsideHours { doctor_name: String },
    OffGrid,
}

/// Free slots for one clinic-local day. A slot whose start is exactly `clinic.now`
/// is still free: it has not started in the past.
pub fn availability(
    clinic: &ClinicConfig,
    date: NaiveDate,
    doctor_id: Option<&str>,
    taken: &HashSet<(String, i64)>,
) -> Result<Day, Reject> {
    let doctors = selected_doctors(clinic, doctor_id)?;
    if clinic.closed_dates.contains(&date) {
        return Ok(Day {
            closed: true,
            slots: Vec::new(),
        });
    }

    let mut slots = Vec::new();
    for doctor in doctors {
        for slot in slots_on(clinic, doctor, date) {
            if slot.start < clinic.now {
                continue;
            }
            let key = (slot.doctor_id.clone(), slot.start.timestamp());
            if taken.contains(&key) {
                continue;
            }
            slots.push(slot);
        }
    }
    Ok(Day {
        closed: false,
        slots,
    })
}

/// Free slots for this doctor whose start is within 3 hours of `around`.
/// The requested instant itself is left out, so a taken slot is not offered back.
pub fn nearby_slots(
    clinic: &ClinicConfig,
    doctor_id: &str,
    around: DateTime<FixedOffset>,
    taken: &HashSet<(String, i64)>,
) -> Vec<Slot> {
    let window = TimeDelta::hours(3);
    let earliest = around - window;
    let latest = around + window;
    let mut dates = vec![earliest.date_naive()];
    for date in [around.date_naive(), latest.date_naive()] {
        if !dates.contains(&date) {
            dates.push(date);
        }
    }

    let mut slots = Vec::new();
    for date in dates {
        let Ok(day) = availability(clinic, date, Some(doctor_id), taken) else {
            continue;
        };
        slots.extend(day.slots.into_iter().filter(|slot| {
            slot.start >= earliest
                && slot.start <= latest
                && slot.start.timestamp() != around.timestamp()
        }));
    }
    slots.sort_by_key(|slot| slot.start.timestamp());
    slots
}

/// Accept a start instant only when it is one generated slot for that doctor.
pub fn check_slot(
    clinic: &ClinicConfig,
    doctor_id: &str,
    start: DateTime<FixedOffset>,
) -> Result<Slot, Reject> {
    let doctor = find_doctor(clinic, doctor_id).ok_or(Reject::UnknownDoctor)?;
    let start = in_clinic(clinic, start);
    let date = start.date_naive();
    if clinic.closed_dates.contains(&date) {
        return Err(Reject::Closed { date });
    }

    if let Some(slot) = slots_on(clinic, doctor, date)
        .into_iter()
        .find(|slot| slot.start.timestamp() == start.timestamp())
    {
        if slot.start < clinic.now {
            return Err(Reject::Past);
        }
        return Ok(slot);
    }

    let time = start.time();
    let ranges = hours_on(doctor, date);
    if ranges
        .iter()
        .any(|range| time >= range.start && time < range.end)
    {
        return Err(Reject::OffGrid);
    }
    Err(Reject::OutsideHours {
        doctor_name: doctor.name.clone(),
    })
}

fn selected_doctors<'a>(
    clinic: &'a ClinicConfig,
    doctor_id: Option<&str>,
) -> Result<Vec<&'a Doctor>, Reject> {
    match doctor_id {
        None => Ok(clinic.doctors.iter().collect()),
        Some(id) => find_doctor(clinic, id)
            .map(|doctor| vec![doctor])
            .ok_or(Reject::UnknownDoctor),
    }
}

fn find_doctor<'a>(clinic: &'a ClinicConfig, doctor_id: &str) -> Option<&'a Doctor> {
    clinic.doctors.iter().find(|doctor| doctor.id == doctor_id)
}

fn slots_on(clinic: &ClinicConfig, doctor: &Doctor, date: NaiveDate) -> Vec<Slot> {
    let Some(step) = slot_step(clinic.slot_minutes) else {
        return Vec::new();
    };
    let offset = clinic.now.offset();
    let mut slots = Vec::new();
    for range in hours_on(doctor, date) {
        for time in start_times(*range, clinic.slot_minutes) {
            let local = date.and_time(time);
            let Some(start) = offset.from_local_datetime(&local).single() else {
                continue;
            };
            slots.push(Slot {
                doctor_id: doctor.id.clone(),
                doctor_name: doctor.name.clone(),
                start,
                end: start + step,
            });
        }
    }
    slots
}

fn hours_on(doctor: &Doctor, date: NaiveDate) -> &[TimeRange] {
    doctor
        .hours
        .get(weekday_key(date))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn start_times(range: TimeRange, slot_minutes: u32) -> Vec<NaiveTime> {
    if slot_minutes == 0 {
        return Vec::new();
    }
    let mut times = Vec::new();
    let mut cursor = minutes(range.start);
    let end = minutes(range.end);
    while cursor + slot_minutes <= end {
        times.push(time_from_minutes(cursor));
        cursor += slot_minutes;
    }
    times
}

fn minutes(time: NaiveTime) -> u32 {
    time.hour() * 60 + time.minute()
}

fn time_from_minutes(mins: u32) -> NaiveTime {
    NaiveTime::from_num_seconds_from_midnight_opt(mins * 60, 0).expect("minutes fit in a day")
}

fn slot_step(slot_minutes: u32) -> Option<TimeDelta> {
    if slot_minutes == 0 {
        None
    } else {
        Some(TimeDelta::minutes(i64::from(slot_minutes)))
    }
}

fn in_clinic(clinic: &ClinicConfig, when: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    when.with_timezone(clinic.now.offset())
}

fn weekday_key(date: NaiveDate) -> &'static str {
    match date.weekday() {
        chrono::Weekday::Mon => "mon",
        chrono::Weekday::Tue => "tue",
        chrono::Weekday::Wed => "wed",
        chrono::Weekday::Thu => "thu",
        chrono::Weekday::Fri => "fri",
        chrono::Weekday::Sat => "sat",
        chrono::Weekday::Sun => "sun",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ClinicConfig;
    use chrono::{DateTime, FixedOffset, NaiveDate};
    use std::collections::HashSet;

    fn clinic() -> ClinicConfig {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("clinic.json");
        crate::read_clinic(path).unwrap()
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn at(text: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(text).unwrap()
    }

    #[test]
    fn tuesday_lists_both_doctors_on_the_grid() {
        let clinic = clinic();
        let day = availability(&clinic, date("2026-11-17"), None, &HashSet::new()).unwrap();
        assert!(!day.closed);
        assert_eq!(day.slots.len(), 24);
        assert_eq!(day.slots[0].doctor_id, "rao");
        assert_eq!(day.slots[0].start, at("2026-11-17T10:00:00+05:30"));
        assert_eq!(day.slots[0].end, at("2026-11-17T10:30:00+05:30"));
        assert!(day.slots.iter().any(|slot| {
            slot.doctor_id == "iyer" && slot.start == at("2026-11-17T16:30:00+05:30")
        }));
    }

    #[test]
    fn monday_at_opening_keeps_the_current_slot() {
        let clinic = clinic();
        let day = availability(&clinic, date("2026-11-16"), Some("rao"), &HashSet::new()).unwrap();
        assert_eq!(day.slots.len(), 12);
        assert_eq!(day.slots[0].start, at("2026-11-16T10:00:00+05:30"));
    }

    #[test]
    fn slots_that_already_started_are_omitted() {
        let mut clinic = clinic();
        clinic.now = at("2026-11-16T10:15:00+05:30");
        let day = availability(&clinic, date("2026-11-16"), Some("rao"), &HashSet::new()).unwrap();
        assert_eq!(day.slots.len(), 11);
        assert_eq!(day.slots[0].start, at("2026-11-16T10:30:00+05:30"));
        assert!(matches!(
            check_slot(&clinic, "rao", at("2026-11-16T10:00:00+05:30")),
            Err(Reject::Past)
        ));
    }

    #[test]
    fn saturday_is_rao_morning_only() {
        let clinic = clinic();
        let day = availability(&clinic, date("2026-11-21"), None, &HashSet::new()).unwrap();
        assert_eq!(day.slots.len(), 6);
        assert!(day.slots.iter().all(|slot| slot.doctor_id == "rao"));
    }

    #[test]
    fn closed_date_has_no_slots() {
        let clinic = clinic();
        let day = availability(&clinic, date("2026-11-20"), None, &HashSet::new()).unwrap();
        assert!(day.closed);
        assert!(day.slots.is_empty());
        assert_eq!(
            check_slot(&clinic, "rao", at("2026-11-20T10:00:00+05:30")),
            Err(Reject::Closed {
                date: date("2026-11-20")
            })
        );
    }

    #[test]
    fn rejects_unknown_past_off_grid_and_outside_hours() {
        let clinic = clinic();
        assert_eq!(
            check_slot(&clinic, "khan", at("2026-11-17T10:00:00+05:30")),
            Err(Reject::UnknownDoctor)
        );
        assert_eq!(
            check_slot(&clinic, "rao", at("2026-11-14T10:00:00+05:30")),
            Err(Reject::Past)
        );
        assert_eq!(
            check_slot(&clinic, "rao", at("2026-11-21T10:15:00+05:30")),
            Err(Reject::OffGrid)
        );
        assert!(matches!(
            check_slot(&clinic, "iyer", at("2026-11-21T11:00:00+05:30")),
            Err(Reject::OutsideHours { .. })
        ));
        assert!(matches!(
            check_slot(&clinic, "rao", at("2026-11-17T13:00:00+05:30")),
            Err(Reject::OutsideHours { .. })
        ));
    }

    #[test]
    fn utc_instant_matches_the_clinic_slot() {
        let clinic = clinic();
        let slot = check_slot(&clinic, "rao", at("2026-11-18T06:30:00Z")).unwrap();
        assert_eq!(slot.start, at("2026-11-18T12:00:00+05:30"));
        assert_eq!(slot.doctor_name, "Dr. Rao");
    }

    #[test]
    fn nearby_slots_stay_inside_three_hours() {
        let clinic = clinic();
        let around = at("2026-11-18T12:00:00+05:30");
        let taken = HashSet::from([("rao".to_string(), around.timestamp())]);
        let starts: Vec<_> = nearby_slots(&clinic, "rao", around, &taken)
            .into_iter()
            .map(|slot| slot.start)
            .collect();
        assert_eq!(
            starts,
            vec![
                at("2026-11-18T10:00:00+05:30"),
                at("2026-11-18T10:30:00+05:30"),
                at("2026-11-18T11:00:00+05:30"),
                at("2026-11-18T11:30:00+05:30"),
                at("2026-11-18T12:30:00+05:30"),
            ]
        );
    }

    #[test]
    fn taken_slots_are_hidden() {
        let clinic = clinic();
        let start = at("2026-11-18T12:00:00+05:30");
        let taken = HashSet::from([("rao".to_string(), start.timestamp())]);
        let day = availability(&clinic, date("2026-11-18"), None, &taken).unwrap();
        assert!(!day.slots.iter().any(|slot| {
            slot.doctor_id == "rao" && slot.start.timestamp() == start.timestamp()
        }));
        assert!(day.slots.iter().any(|slot| {
            slot.doctor_id == "iyer" && slot.start == at("2026-11-18T12:00:00+05:30")
        }));
    }
}
