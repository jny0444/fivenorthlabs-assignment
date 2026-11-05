use chrono::NaiveDate;
use fivestart_assignment::models::{ClinicConfig, Message};
use fivestart_assignment::read_clinic;
use std::fs;
use std::path::PathBuf;

fn manifest(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name)
}

#[test]
fn clinic_and_messages_match_the_files() {
    let clinic: ClinicConfig = read_clinic(manifest("clinic.json")).unwrap();
    let messages: Vec<Message> =
        serde_json::from_str(&fs::read_to_string(manifest("messages.json")).unwrap()).unwrap();

    assert_eq!(clinic.clinic, "Sunrise Dental");
    assert_eq!(clinic.timezone, "Asia/Kolkata");
    assert_eq!(clinic.slot_minutes, 30);
    assert_eq!(clinic.doctors.len(), 2);
    assert_eq!(
        clinic.closed_dates,
        vec![NaiveDate::from_ymd_opt(2026, 11, 20).unwrap()]
    );
    assert_eq!(clinic.now.to_rfc3339(), "2026-11-16T10:00:00+05:30");

    let rao = clinic
        .doctors
        .iter()
        .find(|doctor| doctor.id == "rao")
        .unwrap();
    assert_eq!(rao.hours["mon"].len(), 2);
    assert_eq!(rao.hours["mon"][0].start.to_string(), "10:00:00");
    assert_eq!(rao.hours["mon"][0].end.to_string(), "13:00:00");
    assert_eq!(rao.hours["sun"].len(), 0);

    let iyer = clinic
        .doctors
        .iter()
        .find(|doctor| doctor.id == "iyer")
        .unwrap();
    assert_eq!(iyer.hours["sat"].len(), 0);
    assert_eq!(iyer.hours["mon"].len(), 1);

    assert_eq!(messages.len(), 15);
    assert_eq!(messages[0].id, "m01");
    assert_eq!(messages[0].from, "+919800000001");
    assert!(messages[6].sent_at_same_time_as.is_none());
    assert_eq!(messages[8].sent_at_same_time_as.as_deref(), Some("m10"));
    assert_eq!(messages[9].sent_at_same_time_as.as_deref(), Some("m09"));
}
