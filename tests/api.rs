use fivestart_assignment::api;
use fivestart_assignment::read_clinic;
use fivestart_assignment::store::Store;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::path::Path;
use tokio::net::TcpListener;

struct App {
    base: String,
    client: reqwest::Client,
}

impl App {
    async fn spawn() -> Self {
        let clinic =
            read_clinic(Path::new(env!("CARGO_MANIFEST_DIR")).join("clinic.json")).unwrap();
        let store = Store::in_memory().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            api::serve(clinic, store, listener).await.unwrap();
        });
        let app = Self {
            base: format!("http://{addr}"),
            client: reqwest::Client::new(),
        };
        for _ in 0..50 {
            if app
                .client
                .get(format!("{}/slots?date=2026-11-18", app.base))
                .send()
                .await
                .is_ok()
            {
                return app;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("server did not accept connections");
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let response = self
            .client
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    }
}

#[tokio::test]
async fn lists_free_slots_and_hides_a_booked_one() {
    let app = App::spawn().await;
    let (status, body) = app.get("/slots?date=2026-11-17&doctor_id=rao").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["closed"], false);
    assert_eq!(body["slots"].as_array().unwrap().len(), 12);

    let (status, booked) = app
        .post(
            "/bookings",
            json!({
                "phone": "+919800000001",
                "doctor_id": "rao",
                "start": "2026-11-17T11:00:00+05:30"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(booked["doctor_name"], "Dr. Rao");
    assert_eq!(booked["end"], "2026-11-17T11:30:00+05:30");

    let (_, slots) = app.get("/slots?date=2026-11-17&doctor_id=rao").await;
    assert!(
        !slots["slots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|slot| { slot["start"] == "2026-11-17T11:00:00+05:30" })
    );

    let (status, day) = app.get("/bookings?date=2026-11-17").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(day.as_array().unwrap().len(), 1);
    assert_eq!(day[0]["phone"], "+919800000001");
}

#[tokio::test]
async fn rules_reject_bad_bookings() {
    let app = App::spawn().await;
    let cases = [
        (
            json!({"phone": "+919800000001", "doctor_id": "rao", "start": "2026-11-20T10:00:00+05:30"}),
            "closed",
        ),
        (
            json!({"phone": "+919800000001", "doctor_id": "rao", "start": "2026-11-14T10:00:00+05:30"}),
            "past",
        ),
        (
            json!({"phone": "+919800000001", "doctor_id": "rao", "start": "2026-11-18T12:15:00+05:30"}),
            "slot boundary",
        ),
        (
            json!({"phone": "+919800000001", "doctor_id": "iyer", "start": "2026-11-21T11:00:00+05:30"}),
            "not working",
        ),
        (
            json!({"phone": "+919800000001", "doctor_id": "khan", "start": "2026-11-18T12:00:00+05:30"}),
            "unknown doctor",
        ),
        (
            json!({"phone": "  ", "doctor_id": "rao", "start": "2026-11-18T12:00:00+05:30"}),
            "phone is required",
        ),
    ];
    for (body, needle) in cases {
        let (status, response) = app.post("/bookings", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert!(
            response["error"].as_str().unwrap().contains(needle),
            "{response}"
        );
    }

    let (status, response) = app.get("/slots?date=2026-11-20").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["closed"], true);
    assert_eq!(response["slots"].as_array().unwrap().len(), 0);

    let (status, response) = app.get("/slots").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(response["error"].as_str().unwrap().contains("date"));
}

#[tokio::test]
async fn taken_slot_offers_openings_within_three_hours() {
    let app = App::spawn().await;
    let (status, _) = app
        .post(
            "/bookings",
            json!({
                "phone": "+919800000001",
                "doctor_id": "rao",
                "start": "2026-11-18T12:00:00+05:30"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = app
        .post(
            "/bookings",
            json!({
                "phone": "+919800000002",
                "doctor_id": "rao",
                "start": "2026-11-18T12:00:00+05:30"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("within 3 hours"));
    let starts: Vec<&str> = body["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|slot| slot["start"].as_str().unwrap())
        .collect();
    assert_eq!(
        starts,
        vec![
            "2026-11-18T10:00:00+05:30",
            "2026-11-18T10:30:00+05:30",
            "2026-11-18T11:00:00+05:30",
            "2026-11-18T11:30:00+05:30",
            "2026-11-18T12:30:00+05:30",
        ]
    );
}

#[tokio::test]
async fn parallel_requests_cannot_double_book() {
    let app = App::spawn().await;
    let mut tasks = Vec::new();
    for n in 0..8 {
        let client = app.client.clone();
        let url = format!("{}/bookings", app.base);
        tasks.push(tokio::spawn(async move {
            client
                .post(url)
                .json(&json!({
                    "phone": format!("+9198000002{n:02}"),
                    "doctor_id": "rao",
                    "start": "2026-11-18T12:00:00+05:30"
                }))
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    let mut created = 0;
    let mut conflict = 0;
    for task in tasks {
        match task.await.unwrap() {
            StatusCode::CREATED => created += 1,
            StatusCode::CONFLICT => conflict += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!(created, 1);
    assert_eq!(conflict, 7);

    let (_, bookings) = app.get("/bookings?date=2026-11-18").await;
    let rows = bookings.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["doctor_id"], "rao");
    assert_eq!(rows[0]["start"], "2026-11-18T12:00:00+05:30");
}

#[tokio::test]
async fn parallel_requests_for_two_doctors_both_book() {
    let app = App::spawn().await;
    let rao = app.client.clone();
    let iyer = app.client.clone();
    let rao_url = format!("{}/bookings", app.base);
    let iyer_url = rao_url.clone();
    let (rao, iyer) = tokio::join!(
        rao.post(rao_url)
            .json(&json!({
                "phone": "+919800000001",
                "doctor_id": "rao",
                "start": "2026-11-18T11:00"
            }))
            .send(),
        iyer.post(iyer_url)
            .json(&json!({
                "phone": "+919800000002",
                "doctor_id": "iyer",
                "start": "2026-11-18T11:00:00+05:30"
            }))
            .send(),
    );
    assert_eq!(rao.unwrap().status(), StatusCode::CREATED);
    assert_eq!(iyer.unwrap().status(), StatusCode::CREATED);
    let (_, bookings) = app.get("/bookings?date=2026-11-18").await;
    assert_eq!(bookings.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn cancel_is_limited_to_the_booking_phone() {
    let app = App::spawn().await;
    let (status, booked) = app
        .post(
            "/bookings",
            json!({
                "phone": "+919800000001",
                "doctor_id": "rao",
                "start": "2026-11-19T17:00:00+05:30"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = booked["id"].as_i64().unwrap();

    let (status, body) = app
        .post(
            &format!("/bookings/{id}/cancel"),
            json!({ "phone": "+919800000099" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"].as_str().unwrap().contains("phone number"));
    let (_, still_there) = app.get("/bookings?date=2026-11-19").await;
    assert_eq!(still_there.as_array().unwrap().len(), 1);

    let (status, cancelled) = app
        .post(
            &format!("/bookings/{id}/cancel"),
            json!({ "phone": "+919800000001" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["status"], "cancelled");
    let (_, empty) = app.get("/bookings?date=2026-11-19").await;
    assert!(empty.as_array().unwrap().is_empty());

    let (status, _) = app
        .post(
            "/bookings",
            json!({
                "phone": "+919800000003",
                "doctor_id": "rao",
                "start": "2026-11-19T17:00:00+05:30"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, missing) = app
        .post("/bookings/999/cancel", json!({ "phone": "+919800000001" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["error"], "booking not found");
}
