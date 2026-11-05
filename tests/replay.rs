use fivestart_assignment::api;
use fivestart_assignment::read_clinic;
use fivestart_assignment::replay;
use fivestart_assignment::store::Store;
use std::path::Path;

#[tokio::test]
async fn mock_messages_get_plain_replies() {
    let clinic = read_clinic(Path::new(env!("CARGO_MANIFEST_DIR")).join("clinic.json")).unwrap();
    let store = Store::in_memory().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        api::serve(clinic, store, listener).await.unwrap();
    });
    let client = reqwest::Client::new();
    for _ in 0..50 {
        if client
            .get(format!("{base}/slots?date=2026-11-18"))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report = replay::play(
        &base,
        &root.join("messages.json"),
        &root.join("mock.json"),
        &root.join("expected.json"),
    )
    .await
    .unwrap();
    let lines = &report.lines;
    assert_eq!(lines.len(), 15);
    let reply = |id: &str| {
        lines
            .iter()
            .find(|line| line.id == id)
            .unwrap()
            .reply
            .clone()
    };

    assert!(reply("m01").starts_with("You're booked with Dr. Rao"));
    assert!(reply("m02").contains("6 PM"));
    assert!(reply("m03").ends_with('?'));
    assert!(reply("m04").contains("You're booked with Dr. Iyer"));
    assert!(reply("m05").contains("Dr. Iyer is not working then"));
    assert!(reply("m05").contains("Dr. Rao"));
    assert!(!reply("m05").contains("You're booked"));
    assert!(reply("m06").contains("7:30 PM"));
    assert!(!reply("m06").contains("You're booked"));
    assert!(reply("m07").contains("not open"));
    assert!(!reply("m07").contains("You're booked"));
    assert!(reply("m08").contains("already passed"));
    let noon = [reply("m09"), reply("m10")];
    assert_eq!(
        noon.iter()
            .filter(|text| text.contains("You're booked"))
            .count(),
        1
    );
    assert_eq!(
        noon.iter()
            .filter(|text| text.contains("just taken"))
            .count(),
        1
    );
    assert!(reply("m11").starts_with("Cancelled your appointment with Dr. Rao"));
    assert!(reply("m12").contains("another phone number"));
    assert!(!reply("m12").contains("Cancelled"));
    assert!(reply("m13").starts_with("You're booked with Dr. Rao"));
    assert!(reply("m14").ends_with('?'));
    assert!(reply("m15").contains("You're booked with Dr. Rao"));
    assert!(reply("m15").contains("10 AM"));

    assert_eq!(report.score.wrong_bookings, 0);
    assert_eq!(report.score.unnecessary_questions, 0);
    assert_eq!(report.score.handled_correctly, 15);
    assert_eq!(report.score.total, 15);
    assert!(report.score.misses.is_empty(), "{:?}", report.score.misses);
}
