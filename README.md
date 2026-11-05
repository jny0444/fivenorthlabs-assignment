# Sunrise Dental bookings

A small API that books WhatsApp appointments for two doctors. Clinic rules live in Rust. `mock.json` is the model's reading of the 15 messages. This run does not call an LLM, because no API key is set.

The clock is `now` in `clinic.json`: `2026-11-16T10:00:00+05:30`. It is not the real time.

## Run

```bash
cargo test
cargo run -- replay
cargo run
```

The first build compiles SQLite and can take about a minute. After that, each command finishes in a few seconds.

`replay` starts the API, posts every row of `mock.json` to `POST /messages` with the phone number from `messages.json`, and prints the plain-text reply. `m09` and `m10` are sent at the same time. It then prints:

- wrong bookings
- questions asked when the request was clear enough to book
- messages handled correctly

The outcomes it checks against are in `expected.json`.

`cargo run` serves `http://127.0.0.1:3000` and stores appointments in `bookings.db`.

## Endpoints

- `GET /slots?date=YYYY-MM-DD&doctor_id=rao` lists free 30-minute slots. `doctor_id` is optional.
- `POST /bookings` with `{"phone","doctor_id","start"}` books one slot. `start` is RFC3339 or `YYYY-MM-DDTHH:MM` in clinic time.
- `GET /bookings?date=YYYY-MM-DD` lists that day's appointments.
- `POST /bookings/{id}/cancel` with `{"phone"}` cancels only if that phone made the booking.
- `POST /messages` takes the mock fields plus `from`. The response body is one plain sentence: booked, cancelled, refused, or a question.

Doctor ids are `rao` and `iyer`.

`NOTE.md` explains which choices belong to the model and which belong to the code.
