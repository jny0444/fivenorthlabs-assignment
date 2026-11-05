use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;

use crate::decide::{self, Intake};
use crate::models::ClinicConfig;
use crate::schedule::{self, Day, Reject, Slot};
use crate::store::{BookError, Booking, CancelError, Store};

#[derive(Clone)]
struct AppState {
    clinic: Arc<ClinicConfig>,
    store: Store,
}

pub fn router(clinic: ClinicConfig, store: Store) -> Router {
    let state = AppState {
        clinic: Arc::new(clinic),
        store,
    };
    Router::new()
        .route("/", get(index))
        .route("/slots", get(list_slots))
        .route("/bookings", get(list_bookings).post(create_booking))
        .route("/bookings/{id}/cancel", post(cancel_booking))
        .route("/messages", post(handle_message))
        .with_state(state)
}

pub async fn serve(
    clinic: ClinicConfig,
    store: Store,
    listener: TcpListener,
) -> std::io::Result<()> {
    axum::serve(listener, router(clinic, store)).await
}

async fn index(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "clinic": state.clinic.clinic,
        "timezone": state.clinic.timezone,
        "now": state.clinic.now,
        "slot_minutes": state.clinic.slot_minutes,
        "endpoints": {
            "slots": "GET /slots?date=YYYY-MM-DD&doctor_id=rao",
            "book": "POST /bookings {phone, doctor_id, start}",
            "appointments": "GET /bookings?date=YYYY-MM-DD",
            "cancel": "POST /bookings/{id}/cancel {phone}",
            "messages": "POST /messages returns a plain-text reply"
        }
    }))
}

#[derive(Deserialize)]
struct SlotsQuery {
    date: Option<String>,
    doctor_id: Option<String>,
}

async fn list_slots(
    State(state): State<AppState>,
    Query(query): Query<SlotsQuery>,
) -> Result<Json<DayBody>, ApiError> {
    let date = required_date(query.date)?;
    let doctor_id = blank_as_none(query.doctor_id);
    let taken = state.store.taken_on(date).map_err(ApiError::db)?;
    let day = schedule::availability(&state.clinic, date, doctor_id.as_deref(), &taken)
        .map_err(|err| ApiError::from_reject(&state.clinic, err))?;
    Ok(Json(DayBody { date, day }))
}

#[derive(Deserialize)]
struct BookingsQuery {
    date: Option<String>,
}

async fn list_bookings(
    State(state): State<AppState>,
    Query(query): Query<BookingsQuery>,
) -> Result<Json<Vec<Booking>>, ApiError> {
    let date = match query.date {
        Some(text) if !text.trim().is_empty() => Some(parse_date(text.trim())?),
        _ => None,
    };
    state.store.list(date).map(Json).map_err(ApiError::db)
}

#[derive(Deserialize)]
struct BookBody {
    phone: String,
    doctor_id: String,
    start: String,
}

async fn create_booking(
    State(state): State<AppState>,
    Json(body): Json<BookBody>,
) -> Result<(StatusCode, Json<Booking>), ApiError> {
    let start = parse_start(&state.clinic, &body.start)?;
    match state
        .store
        .book(&state.clinic, &body.phone, &body.doctor_id, start)
    {
        Ok(booking) => Ok((StatusCode::CREATED, Json(booking))),
        Err(err) => Err(ApiError::from_book(&state, err)),
    }
}

#[derive(Deserialize)]
struct CancelBody {
    phone: String,
}

#[derive(Deserialize)]
struct MessageIn {
    from: String,
    intent: String,
    date: Option<String>,
    time: Option<String>,
    doctor_id: Option<String>,
    #[serde(default)]
    any_doctor: bool,
    #[serde(default)]
    earliest: bool,
}

async fn handle_message(
    State(state): State<AppState>,
    Json(body): Json<MessageIn>,
) -> Result<String, ApiError> {
    let intake = Intake::parse(
        body.from,
        &body.intent,
        body.date.as_deref(),
        body.time.as_deref(),
        body.doctor_id.as_deref(),
        body.any_doctor,
        body.earliest,
    );
    let outcome = decide::handle(&state.clinic, &state.store, &intake).map_err(ApiError::db)?;
    Ok(outcome.reply)
}

async fn cancel_booking(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<CancelBody>,
) -> Result<Json<Booking>, ApiError> {
    state
        .store
        .cancel(id, &body.phone)
        .map(Json)
        .map_err(ApiError::from_cancel)
}

struct DayBody {
    date: NaiveDate,
    day: Day,
}

impl serde::Serialize for DayBody {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut body = serializer.serialize_struct("Day", 3)?;
        body.serialize_field("date", &self.date)?;
        body.serialize_field("closed", &self.day.closed)?;
        body.serialize_field("slots", &self.day.slots)?;
        body.end()
    }
}

struct ApiError {
    status: StatusCode,
    message: String,
    suggestions: Option<Vec<Slot>>,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            suggestions: None,
        }
    }

    fn db(err: rusqlite::Error) -> Self {
        eprintln!("database error: {err}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not use the bookings database",
        )
    }

    fn from_reject(clinic: &ClinicConfig, err: Reject) -> Self {
        let message = match err {
            Reject::UnknownDoctor => "unknown doctor".to_string(),
            Reject::Closed { date } => format!("the clinic is closed on {date}"),
            Reject::Past => format!(
                "that time is in the past (clinic time is {})",
                clinic.now.format("%Y-%m-%d %H:%M")
            ),
            Reject::OutsideHours { doctor_name } => format!("{doctor_name} is not working then"),
            Reject::OffGrid => format!(
                "appointments are {} minutes and must start on a slot boundary",
                clinic.slot_minutes
            ),
        };
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn from_book(state: &AppState, err: BookError) -> Self {
        match err {
            BookError::EmptyPhone => Self::new(StatusCode::BAD_REQUEST, "phone is required"),
            BookError::Taken(slot) => Self::taken(state, slot),
            BookError::Schedule(reason) => Self::from_reject(&state.clinic, reason),
            BookError::Db(err) => Self::db(err),
        }
    }

    fn taken(state: &AppState, slot: Slot) -> Self {
        let suggestions = match openings_near(state, &slot) {
            Ok(slots) => slots,
            Err(err) => return Self::db(err),
        };
        let message = if suggestions.is_empty() {
            "that slot is already booked. nothing else is free within 3 hours"
        } else {
            "that slot is already booked. these times are free within 3 hours"
        };
        Self {
            status: StatusCode::CONFLICT,
            message: message.to_string(),
            suggestions: Some(suggestions),
        }
    }

    fn from_cancel(err: CancelError) -> Self {
        match err {
            CancelError::NotFound => Self::new(StatusCode::NOT_FOUND, "booking not found"),
            CancelError::NotOwner => Self::new(
                StatusCode::FORBIDDEN,
                "only the phone number that booked can cancel",
            ),
            CancelError::Db(err) => Self::db(err),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.message });
        if let Some(suggestions) = self.suggestions {
            body["suggestions"] = json!(suggestions);
        }
        (self.status, Json(body)).into_response()
    }
}

fn openings_near(state: &AppState, slot: &Slot) -> Result<Vec<Slot>, rusqlite::Error> {
    let window = chrono::TimeDelta::hours(3);
    let mut taken = std::collections::HashSet::new();
    let mut seen = Vec::new();
    for instant in [slot.start - window, slot.start, slot.start + window] {
        let date = instant.date_naive();
        if seen.contains(&date) {
            continue;
        }
        seen.push(date);
        taken.extend(state.store.taken_on(date)?);
    }
    Ok(schedule::nearby_slots(
        &state.clinic,
        &slot.doctor_id,
        slot.start,
        &taken,
    ))
}

fn required_date(date: Option<String>) -> Result<NaiveDate, ApiError> {
    match date {
        Some(text) if !text.trim().is_empty() => parse_date(text.trim()),
        _ => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "date is required (YYYY-MM-DD)",
        )),
    }
}

fn parse_date(text: &str) -> Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "date must be YYYY-MM-DD"))
}

fn parse_start(clinic: &ClinicConfig, text: &str) -> Result<DateTime<FixedOffset>, ApiError> {
    if let Ok(when) = DateTime::parse_from_rfc3339(text) {
        return Ok(when);
    }
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M"))
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "start must be RFC3339 or YYYY-MM-DDTHH:MM",
            )
        })?;
    clinic
        .now
        .offset()
        .from_local_datetime(&naive)
        .single()
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "start is not a real local time"))
}

fn blank_as_none(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let text = text.trim().to_string();
        if text.is_empty() { None } else { Some(text) }
    })
}
