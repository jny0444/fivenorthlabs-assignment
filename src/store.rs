use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, FixedOffset, NaiveDate};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::models::ClinicConfig;
use crate::schedule::{self, Reject, Slot};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Booking {
    pub id: i64,
    pub phone: String,
    pub doctor_id: String,
    pub doctor_name: String,
    pub start: DateTime<FixedOffset>,
    pub end: DateTime<FixedOffset>,
    pub status: String,
}

#[derive(Debug)]
pub enum BookError {
    EmptyPhone,
    Schedule(Reject),
    Taken(Slot),
    Db(rusqlite::Error),
}

#[derive(Debug)]
pub enum CancelError {
    NotFound,
    NotOwner,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for BookError {
    fn from(err: rusqlite::Error) -> Self {
        BookError::Db(err)
    }
}

impl From<rusqlite::Error> for CancelError {
    fn from(err: rusqlite::Error) -> Self {
        CancelError::Db(err)
    }
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &str) -> Result<Self, rusqlite::Error> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, rusqlite::Error> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS bookings (
                id INTEGER PRIMARY KEY,
                phone TEXT NOT NULL,
                doctor_id TEXT NOT NULL,
                doctor_name TEXT NOT NULL,
                start_at TEXT NOT NULL,
                end_at TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('booked', 'cancelled'))
            );
            CREATE UNIQUE INDEX IF NOT EXISTS bookings_one_patient_per_slot
                ON bookings (doctor_id, start_at)
                WHERE status = 'booked';
            ",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn book(
        &self,
        clinic: &ClinicConfig,
        phone: &str,
        doctor_id: &str,
        start: DateTime<FixedOffset>,
    ) -> Result<Booking, BookError> {
        let phone = phone.trim();
        if phone.is_empty() {
            return Err(BookError::EmptyPhone);
        }
        let slot =
            schedule::check_slot(clinic, doctor_id.trim(), start).map_err(BookError::Schedule)?;
        self.insert(phone, &slot)
    }

    /// The mutex and the unique index both cover this insert. Two requests for the
    /// same doctor and start cannot both commit a `booked` row.
    fn insert(&self, phone: &str, slot: &Slot) -> Result<Booking, BookError> {
        let mut conn = self.conn.lock().expect("bookings lock");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stamp = slot.start.to_rfc3339();
        let taken: bool = tx.query_row(
            "
            SELECT EXISTS(
                SELECT 1 FROM bookings
                WHERE status = 'booked' AND doctor_id = ?1 AND start_at = ?2
            )
            ",
            params![slot.doctor_id, stamp],
            |row| row.get(0),
        )?;
        if taken {
            return Err(BookError::Taken(slot.clone()));
        }
        if let Err(err) = tx.execute(
            "
            INSERT INTO bookings (phone, doctor_id, doctor_name, start_at, end_at, status)
            VALUES (?1, ?2, ?3, ?4, ?5, 'booked')
            ",
            params![
                phone,
                slot.doctor_id,
                slot.doctor_name,
                stamp,
                slot.end.to_rfc3339()
            ],
        ) {
            if err.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) {
                return Err(BookError::Taken(slot.clone()));
            }
            return Err(err.into());
        }
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Booking {
            id,
            phone: phone.to_string(),
            doctor_id: slot.doctor_id.clone(),
            doctor_name: slot.doctor_name.clone(),
            start: slot.start,
            end: slot.end,
            status: "booked".to_string(),
        })
    }

    pub fn cancel(&self, id: i64, phone: &str) -> Result<Booking, CancelError> {
        let phone = phone.trim();
        let mut conn = self.conn.lock().expect("bookings lock");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut booking = match tx
            .query_row(
                "
                SELECT id, phone, doctor_id, doctor_name, start_at, end_at, status
                FROM bookings
                WHERE id = ?1 AND status = 'booked'
                ",
                params![id],
                map_booking,
            )
            .optional()?
        {
            Some(booking) => booking,
            None => return Err(CancelError::NotFound),
        };
        if booking.phone != phone {
            return Err(CancelError::NotOwner);
        }
        tx.execute(
            "UPDATE bookings SET status = 'cancelled' WHERE id = ?1 AND status = 'booked'",
            params![id],
        )?;
        tx.commit()?;
        booking.status = "cancelled".to_string();
        Ok(booking)
    }

    pub fn list(&self, date: Option<NaiveDate>) -> Result<Vec<Booking>, rusqlite::Error> {
        let conn = self.conn.lock().expect("bookings lock");
        let mut bookings = conn
            .prepare(
                "
                SELECT id, phone, doctor_id, doctor_name, start_at, end_at, status
                FROM bookings
                WHERE status = 'booked'
                ORDER BY start_at, doctor_id, id
                ",
            )?
            .query_map([], map_booking)?
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(date) = date {
            bookings.retain(|booking| booking.start.date_naive() == date);
        }
        Ok(bookings)
    }

    pub fn taken_on(&self, date: NaiveDate) -> Result<HashSet<(String, i64)>, rusqlite::Error> {
        let taken = self
            .list(Some(date))?
            .into_iter()
            .map(|booking| (booking.doctor_id, booking.start.timestamp()))
            .collect();
        Ok(taken)
    }
}

fn map_booking(row: &rusqlite::Row<'_>) -> rusqlite::Result<Booking> {
    let start: String = row.get(4)?;
    let end: String = row.get(5)?;
    Ok(Booking {
        id: row.get(0)?,
        phone: row.get(1)?,
        doctor_id: row.get(2)?,
        doctor_name: row.get(3)?,
        start: parse_stamp(&start)?,
        end: parse_stamp(&end)?,
        status: row.get(6)?,
    })
}

fn parse_stamp(text: &str) -> rusqlite::Result<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(text).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(err))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ClinicConfig;
    use chrono::{DateTime, FixedOffset};

    fn clinic() -> ClinicConfig {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("clinic.json");
        crate::read_clinic(path).unwrap()
    }

    fn at(text: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(text).unwrap()
    }

    #[test]
    fn second_patient_cannot_take_the_same_slot() {
        let clinic = clinic();
        let store = Store::in_memory().unwrap();
        let start = at("2026-11-18T12:00:00+05:30");
        let first = store.book(&clinic, "+919800000009", "rao", start).unwrap();
        assert_eq!(first.doctor_name, "Dr. Rao");
        assert!(matches!(
            store.book(&clinic, "+919800000010", "rao", start),
            Err(BookError::Taken(_))
        ));
        let iyer = store.book(&clinic, "+919800000010", "iyer", start).unwrap();
        assert_eq!(iyer.doctor_id, "iyer");
        assert_eq!(store.list(None).unwrap().len(), 2);
    }

    #[test]
    fn only_the_booking_phone_can_cancel() {
        let clinic = clinic();
        let store = Store::in_memory().unwrap();
        let start = at("2026-11-17T18:00:00+05:30");
        let booking = store
            .book(&clinic, " +919800000001 ", "rao", start)
            .unwrap();
        assert_eq!(booking.phone, "+919800000001");
        assert!(matches!(
            store.cancel(booking.id, "+919800000099"),
            Err(CancelError::NotOwner)
        ));
        assert_eq!(store.list(None).unwrap().len(), 1);
        let cancelled = store.cancel(booking.id, "+919800000001").unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert!(store.list(None).unwrap().is_empty());
        store.book(&clinic, "+919800000002", "rao", start).unwrap();
        assert_eq!(store.list(None).unwrap().len(), 1);
    }

    #[test]
    fn parallel_book_calls_leave_one_row() {
        let clinic = clinic();
        let store = Store::in_memory().unwrap();
        let start = at("2026-11-18T12:00:00+05:30");
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for n in 0..8 {
            let store = store.clone();
            let clinic = clinic.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                store.book(&clinic, &format!("+9198000001{n:02}"), "rao", start)
            }));
        }
        let mut booked = 0;
        let mut taken = 0;
        for handle in handles {
            match handle.join().unwrap() {
                Ok(_) => booked += 1,
                Err(BookError::Taken(_)) => taken += 1,
                Err(other) => panic!("unexpected book error: {other:?}"),
            }
        }
        assert_eq!(booked, 1);
        assert_eq!(taken, 7);
        assert_eq!(store.list(None).unwrap().len(), 1);
    }
}
