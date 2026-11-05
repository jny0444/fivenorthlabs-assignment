use std::io::{Error, ErrorKind};
use std::path::Path;

use fivestart_assignment::api;
use fivestart_assignment::read_clinic;
use fivestart_assignment::replay;
use fivestart_assignment::store::Store;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("replay") {
        return replay_messages().await;
    }
    serve().await
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let clinic = read_clinic("clinic.json")?;
    let store = Store::open("bookings.db")?;
    let addr = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = TcpListener::bind(&addr).await.map_err(|err| {
        if err.kind() == ErrorKind::AddrInUse {
            Error::new(err.kind(), format!("{addr} is already in use"))
        } else {
            err
        }
    })?;
    println!("booking api on http://{addr}");
    println!("  GET  /slots?date=YYYY-MM-DD&doctor_id=rao");
    println!("  POST /bookings");
    println!("  GET  /bookings?date=YYYY-MM-DD");
    println!("  POST /bookings/{{id}}/cancel");
    println!("  POST /messages");
    api::serve(clinic, store, listener).await?;
    Ok(())
}

async fn replay_messages() -> Result<(), Box<dyn std::error::Error>> {
    let clinic = read_clinic("clinic.json")?;
    let store = Store::in_memory()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move {
        api::serve(clinic, store, listener).await.unwrap();
    });
    let client = reqwest::Client::new();
    let mut ready = false;
    for _ in 0..50 {
        if client
            .get(format!("{base}/slots?date=2026-11-18"))
            .send()
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    if !ready {
        return Err("booking api did not start".into());
    }
    println!("POST {base}/messages\n");
    let report = replay::play(
        &base,
        Path::new("messages.json"),
        Path::new("mock.json"),
        Path::new("expected.json"),
    )
    .await
    .map_err(|err| std::io::Error::other(err.to_string()))?;
    for line in &report.lines {
        println!("{}\n{}\n", line.id, line.reply);
    }
    let score = &report.score;
    println!("wrong bookings: {}", score.wrong_bookings);
    println!(
        "questions asked when the request was clear enough to book: {}",
        score.unnecessary_questions
    );
    println!(
        "messages handled correctly: {}/{}",
        score.handled_correctly, score.total
    );
    for miss in &score.misses {
        println!("{miss}");
    }
    Ok(())
}
