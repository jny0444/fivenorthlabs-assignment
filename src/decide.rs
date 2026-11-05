use std::collections::HashSet;

use chrono::{
    DateTime, Datelike, FixedOffset, NaiveDate, NaiveTime, TimeDelta, TimeZone, Timelike,
};

use crate::models::ClinicConfig;
use crate::schedule::{self, Reject, Slot};
use crate::store::{BookError, Booking, CancelError, Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Booked,
    Cancelled,
    Refused,
    Asked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub action: Action,
    pub reply: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Book,
    Cancel,
    Other,
}

#[derive(Debug, Clone)]
pub struct Intake {
    pub phone: String,
    pub intent: Intent,
    pub date: Option<NaiveDate>,
    pub time: Option<NaiveTime>,
    pub doctor_id: Option<String>,
    pub any_doctor: bool,
    pub earliest: bool,
    pub unreadable: Option<String>,
}

impl Intake {
    pub fn parse(
        phone: String,
        intent: &str,
        date: Option<&str>,
        time: Option<&str>,
        doctor_id: Option<&str>,
        any_doctor: bool,
        earliest: bool,
    ) -> Self {
        let mut unreadable = None;
        let intent = match intent.trim().to_ascii_lowercase().as_str() {
            "book" => Intent::Book,
            "cancel" => Intent::Cancel,
            "other" => Intent::Other,
            _ => {
                unreadable = Some("I couldn't tell if that was a booking or a cancellation".into());
                Intent::Other
            }
        };
        let date = match optional_date(date) {
            Ok(date) => date,
            Err(()) => {
                unreadable = Some("I couldn't read that date".into());
                None
            }
        };
        let time = match optional_time(time) {
            Ok(time) => time,
            Err(()) => {
                unreadable = Some("I couldn't read that time".into());
                None
            }
        };
        let doctor_id = doctor_id
            .map(str::trim)
            .filter(|doctor| !doctor.is_empty())
            .map(str::to_string);
        Self {
            phone,
            intent,
            date,
            time,
            doctor_id,
            any_doctor,
            earliest,
            unreadable,
        }
    }
}

pub fn handle(
    clinic: &ClinicConfig,
    store: &Store,
    intake: &Intake,
) -> Result<Outcome, rusqlite::Error> {
    if intake.phone.trim().is_empty() {
        return Ok(refused("A phone number is required"));
    }
    if let Some(note) = &intake.unreadable {
        return Ok(refused(note));
    }
    match intake.intent {
        Intent::Book => book(clinic, store, intake),
        Intent::Cancel => cancel(store, intake),
        Intent::Other => Ok(asked("Do you want to book an appointment or cancel one?")),
    }
}

fn book(clinic: &ClinicConfig, store: &Store, intake: &Intake) -> Result<Outcome, rusqlite::Error> {
    let named = intake.doctor_id.as_deref().filter(|_| !intake.any_doctor);
    if let Some(doctor_id) = named {
        if !clinic.doctors.iter().any(|doctor| doctor.id == doctor_id) {
            let names = join_words(
                &clinic
                    .doctors
                    .iter()
                    .map(|doctor| doctor.name.clone())
                    .collect::<Vec<_>>(),
                "and",
            );
            return Ok(refused(&format!(
                "I don't have that doctor. The doctors here are {names}"
            )));
        }
    }

    if intake.earliest && intake.date.is_none() && intake.time.is_none() {
        return book_earliest(clinic, store, &intake.phone, named);
    }

    let (Some(date), Some(time)) = (intake.date, intake.time) else {
        return Ok(ask_for_missing(intake));
    };
    let start = at_clinic(clinic, date, time);
    if let Some(doctor_id) = named {
        return book_slot(clinic, store, &intake.phone, doctor_id, start);
    }

    let taken = store.taken_on(date)?;
    let mut open = Vec::new();
    for doctor in &clinic.doctors {
        if let Ok(slot) = schedule::check_slot(clinic, &doctor.id, start) {
            let key = (doctor.id.clone(), slot.start.timestamp());
            if !taken.contains(&key) {
                open.push(slot);
            }
        }
    }
    match open.as_slice() {
        [slot] => book_slot(clinic, store, &intake.phone, &slot.doctor_id, slot.start),
        [] => {
            let reason = reason_for_nobody(clinic, start);
            refuse_with(clinic, store, None, start, &reason)
        }
        several => {
            let names = join_words(
                &several
                    .iter()
                    .map(|slot| slot.doctor_name.clone())
                    .collect::<Vec<_>>(),
                "and",
            );
            Ok(asked(&format!(
                "{names} are both free then. Which doctor should I book?"
            )))
        }
    }
}

fn book_earliest(
    clinic: &ClinicConfig,
    store: &Store,
    phone: &str,
    doctor_id: Option<&str>,
) -> Result<Outcome, rusqlite::Error> {
    let today = clinic.now.date_naive();
    for offset in 0..45 {
        let Some(date) = today.checked_add_signed(TimeDelta::days(offset)) else {
            break;
        };
        let taken = store.taken_on(date)?;
        let Ok(day) = schedule::availability(clinic, date, doctor_id, &taken) else {
            continue;
        };
        if let Some(slot) = day.slots.into_iter().next() {
            return book_slot(clinic, store, phone, &slot.doctor_id, slot.start);
        }
    }
    Ok(refused("There is no opening in the next month and a half"))
}

fn book_slot(
    clinic: &ClinicConfig,
    store: &Store,
    phone: &str,
    doctor_id: &str,
    start: DateTime<FixedOffset>,
) -> Result<Outcome, rusqlite::Error> {
    match store.book(clinic, phone, doctor_id, start) {
        Ok(booking) => Ok(booked(&booking)),
        Err(BookError::Taken(_)) => refuse_with(
            clinic,
            store,
            Some(doctor_id),
            start,
            "That time was just taken",
        ),
        Err(BookError::Schedule(reason)) => {
            refuse_with(clinic, store, Some(doctor_id), start, &reason_text(&reason))
        }
        Err(BookError::EmptyPhone) => Ok(refused("A phone number is required")),
        Err(BookError::Db(err)) => Err(err),
    }
}

fn cancel(store: &Store, intake: &Intake) -> Result<Outcome, rusqlite::Error> {
    let phone = intake.phone.trim();
    let bookings = store.list(None)?;
    let specified = intake.date.is_some() || intake.time.is_some() || intake.doctor_id.is_some();
    let hits: Vec<&Booking> = bookings
        .iter()
        .filter(|booking| matches_cancel(booking, intake))
        .collect();
    let mine: Vec<&Booking> = hits
        .iter()
        .copied()
        .filter(|booking| booking.phone == phone)
        .collect();

    if !specified {
        return cancel_matches(store, phone, &mine);
    }
    if mine.len() == 1 {
        return finish_cancel(store, phone, mine[0].id);
    }
    if mine.len() > 1 {
        return Ok(asked(&which_appointment(&mine)));
    }
    if !hits.is_empty() {
        return Ok(refused(
            "That appointment is under another phone number, so I can't cancel it",
        ));
    }
    Ok(refused("I can't find that appointment"))
}

fn cancel_matches(
    store: &Store,
    phone: &str,
    mine: &[&Booking],
) -> Result<Outcome, rusqlite::Error> {
    match mine {
        [] => Ok(refused("I can't find an appointment on this number")),
        [booking] => finish_cancel(store, phone, booking.id),
        several => Ok(asked(&which_appointment(several))),
    }
}

fn finish_cancel(store: &Store, phone: &str, id: i64) -> Result<Outcome, rusqlite::Error> {
    match store.cancel(id, phone) {
        Ok(booking) => Ok(Outcome {
            action: Action::Cancelled,
            reply: format!(
                "Cancelled your appointment with {} on {}.",
                booking.doctor_name,
                when(booking.start)
            ),
        }),
        Err(CancelError::NotOwner) => Ok(refused(
            "That appointment is under another phone number, so I can't cancel it",
        )),
        Err(CancelError::NotFound) => Ok(refused("I can't find that appointment")),
        Err(CancelError::Db(err)) => Err(err),
    }
}

fn matches_cancel(booking: &Booking, intake: &Intake) -> bool {
    if let Some(date) = intake.date {
        if booking.start.date_naive() != date {
            return false;
        }
    }
    if let Some(time) = intake.time {
        if booking.start.time() != time {
            return false;
        }
    }
    if let Some(doctor_id) = intake.doctor_id.as_deref() {
        if booking.doctor_id != doctor_id {
            return false;
        }
    }
    true
}

fn which_appointment(bookings: &[&Booking]) -> String {
    let parts = bookings
        .iter()
        .map(|booking| format!("the one on {}", when(booking.start)))
        .collect::<Vec<_>>();
    format!(
        "Which appointment should I cancel, {}?",
        join_words(&parts, "or")
    )
}

fn ask_for_missing(intake: &Intake) -> Outcome {
    match (intake.date, intake.time) {
        (None, None) => asked("What day and time would you like?"),
        (None, Some(time)) => asked(&format!(
            "Which date do you mean for {}?",
            clock_label(time.hour(), time.minute())
        )),
        (Some(date), None) => asked(&format!(
            "What time on {} would you like?",
            format_date(date)
        )),
        (Some(_), Some(_)) => asked("What day and time would you like?"),
    }
}

fn refuse_with(
    clinic: &ClinicConfig,
    store: &Store,
    doctor_id: Option<&str>,
    around: DateTime<FixedOffset>,
    reason: &str,
) -> Result<Outcome, rusqlite::Error> {
    let slots = suggest(clinic, store, doctor_id, around)?;
    Ok(Outcome {
        action: Action::Refused,
        reply: with_openings(reason, &slots),
    })
}

fn suggest(
    clinic: &ClinicConfig,
    store: &Store,
    doctor_id: Option<&str>,
    around: DateTime<FixedOffset>,
) -> Result<Vec<Slot>, rusqlite::Error> {
    let taken = load_taken(store, around)?;
    let window = TimeDelta::hours(3);
    let earliest = around - window;
    let latest = around + window;
    let mut dates = vec![earliest.date_naive()];
    for date in [around.date_naive(), latest.date_naive()] {
        if !dates.contains(&date) {
            dates.push(date);
        }
    }
    let mut nearby = Vec::new();
    for date in dates {
        let Ok(day) = schedule::availability(clinic, date, None, &taken) else {
            continue;
        };
        nearby.extend(day.slots.into_iter().filter(|slot| {
            slot.start >= earliest && slot.start <= latest && !same_slot(slot, doctor_id, around)
        }));
    }
    if !nearby.is_empty() {
        return Ok(closest(nearby, around, 4));
    }

    let not_before = if around < clinic.now {
        clinic.now
    } else {
        around
    };
    let mut forward = Vec::new();
    for offset in 0..14 {
        let Some(date) = around
            .date_naive()
            .checked_add_signed(TimeDelta::days(offset))
        else {
            break;
        };
        let Ok(day) = schedule::availability(clinic, date, None, &taken) else {
            continue;
        };
        forward.extend(
            day.slots
                .into_iter()
                .filter(|slot| slot.start >= not_before && !same_slot(slot, doctor_id, around)),
        );
    }
    if let Some(doctor_id) = doctor_id {
        let preferred: Vec<_> = forward
            .iter()
            .filter(|slot| slot.doctor_id == doctor_id)
            .cloned()
            .collect();
        if !preferred.is_empty() {
            return Ok(closest(preferred, around, 4));
        }
    }
    Ok(closest(forward, around, 4))
}

fn load_taken(
    store: &Store,
    around: DateTime<FixedOffset>,
) -> Result<HashSet<(String, i64)>, rusqlite::Error> {
    let start = around
        .date_naive()
        .checked_sub_signed(TimeDelta::days(1))
        .unwrap_or(around.date_naive());
    let mut taken = HashSet::new();
    for offset in 0..16 {
        let Some(date) = start.checked_add_signed(TimeDelta::days(offset)) else {
            break;
        };
        taken.extend(store.taken_on(date)?);
    }
    Ok(taken)
}

fn closest(mut slots: Vec<Slot>, around: DateTime<FixedOffset>, limit: usize) -> Vec<Slot> {
    slots.sort_by_key(|slot| {
        (
            (slot.start.timestamp() - around.timestamp()).abs(),
            slot.start.timestamp(),
            slot.doctor_id.clone(),
        )
    });
    slots.truncate(limit);
    slots.sort_by_key(|slot| (slot.start.timestamp(), slot.doctor_id.clone()));
    slots
}

fn same_slot(slot: &Slot, doctor_id: Option<&str>, around: DateTime<FixedOffset>) -> bool {
    doctor_id == Some(slot.doctor_id.as_str()) && slot.start.timestamp() == around.timestamp()
}

fn reason_for_nobody(clinic: &ClinicConfig, start: DateTime<FixedOffset>) -> String {
    if clinic.closed_dates.contains(&start.date_naive()) {
        return format!(
            "The clinic is closed on {}",
            format_date(start.date_naive())
        );
    }
    if start < clinic.now {
        return "That time has already passed".to_string();
    }
    let off_grid = clinic.doctors.iter().any(|doctor| {
        matches!(
            schedule::check_slot(clinic, &doctor.id, start),
            Err(Reject::OffGrid)
        )
    });
    if off_grid {
        "That time isn't a 30 minute slot".to_string()
    } else {
        "The clinic is not open then".to_string()
    }
}

fn reason_text(err: &Reject) -> String {
    match err {
        Reject::UnknownDoctor => "I don't have that doctor".to_string(),
        Reject::Closed { .. } => "The clinic is closed then".to_string(),
        Reject::Past => "That time has already passed".to_string(),
        Reject::OutsideHours { doctor_name } => format!("{doctor_name} is not working then"),
        Reject::OffGrid => "That time isn't a 30 minute slot".to_string(),
    }
}

fn booked(booking: &Booking) -> Outcome {
    Outcome {
        action: Action::Booked,
        reply: format!(
            "You're booked with {} on {}.",
            booking.doctor_name,
            when(booking.start)
        ),
    }
}

fn refused(reason: &str) -> Outcome {
    Outcome {
        action: Action::Refused,
        reply: format!("{reason}."),
    }
}

fn asked(question: &str) -> Outcome {
    Outcome {
        action: Action::Asked,
        reply: question.to_string(),
    }
}

fn with_openings(reason: &str, slots: &[Slot]) -> String {
    if slots.is_empty() {
        return format!("{reason}.");
    }
    format!("{reason}. {}", offer(slots))
}

fn offer(slots: &[Slot]) -> String {
    let mut groups: Vec<(String, String, Vec<String>)> = Vec::new();
    for slot in slots {
        let day = format_date(slot.start.date_naive());
        let time = clock_label(slot.start.hour(), slot.start.minute());
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.0 == slot.doctor_name && group.1 == day)
        {
            group.2.push(time);
        } else {
            groups.push((slot.doctor_name.clone(), day, vec![time]));
        }
    }
    let parts = groups
        .into_iter()
        .map(|(name, day, times)| format!("{name} on {day} at {}", join_words(&times, "or")))
        .collect::<Vec<_>>();
    format!("Open times: {}.", parts.join("; "))
}

fn when(start: DateTime<FixedOffset>) -> String {
    format!(
        "{}, {}",
        format_date(start.date_naive()),
        clock_label(start.hour(), start.minute())
    )
}

fn format_date(date: NaiveDate) -> String {
    format!(
        "{} {} {}",
        weekday(date.weekday()),
        date.day(),
        month(date.month())
    )
}

fn clock_label(hour: u32, minute: u32) -> String {
    let (hour, suffix) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        hour => (hour - 12, "PM"),
    };
    if minute == 0 {
        format!("{hour} {suffix}")
    } else {
        format!("{hour}:{minute:02} {suffix}")
    }
}

fn weekday(day: chrono::Weekday) -> &'static str {
    match day {
        chrono::Weekday::Mon => "Mon",
        chrono::Weekday::Tue => "Tue",
        chrono::Weekday::Wed => "Wed",
        chrono::Weekday::Thu => "Thu",
        chrono::Weekday::Fri => "Fri",
        chrono::Weekday::Sat => "Sat",
        chrono::Weekday::Sun => "Sun",
    }
}

fn month(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "",
    }
}

fn join_words(parts: &[String], conjunction: &str) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} {conjunction} {second}"),
        many => {
            let (last, rest) = many.split_last().unwrap();
            format!("{}, {conjunction} {last}", rest.join(", "))
        }
    }
}

fn at_clinic(clinic: &ClinicConfig, date: NaiveDate, time: NaiveTime) -> DateTime<FixedOffset> {
    clinic
        .now
        .offset()
        .from_local_datetime(&date.and_time(time))
        .single()
        .expect("clinic offset has one local time")
}

fn optional_date(value: Option<&str>) -> Result<Option<NaiveDate>, ()> {
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| ()),
    }
}

fn optional_time(value: Option<&str>) -> Result<Option<NaiveTime>, ()> {
    let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    NaiveTime::parse_from_str(text, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(text, "%H:%M:%S"))
        .map(Some)
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clinic() -> ClinicConfig {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("clinic.json");
        crate::read_clinic(path).unwrap()
    }

    #[test]
    fn a_bad_date_is_refused_and_not_booked() {
        let clinic = clinic();
        let store = Store::in_memory().unwrap();
        let intake = Intake::parse(
            "+919800000001".into(),
            "book",
            Some("tomorrow"),
            Some("11:00"),
            Some("rao"),
            false,
            false,
        );
        let outcome = handle(&clinic, &store, &intake).unwrap();
        assert_eq!(outcome.action, Action::Refused);
        assert!(outcome.reply.contains("date"));
        assert!(store.list(None).unwrap().is_empty());
    }
}
