# Note

## How the booking flow works

The flow starts with the message. The phone number says who is writing, and `/messages` reads that message to decide what they want: book, cancel, or something else.

From there it looks up the day on `/slots`, then checks `/bookings` for anything already booked. If that time is free, `POST /bookings` creates the appointment. If it is already taken, which the mutex makes clear because the other request is holding the database, the reply comes back with the free times around the one they asked for, about 2–3 hours either side.

Cancelling goes to `POST /bookings/{id}/cancel`. The id picks the booking, and the phone number is sent with it so only that patient can cancel.

## Who decides what

`mock.json` stands in for the model. It only records intent, date, time, and doctor, plus whether the patient will take any doctor or the earliest slot. There is no LLM call, because no API key is set, so there is also no retry loop. A reading that is not a date, not a time, or not one of the two doctors is refused and does not become a booking.

The code decides whether that reading can be kept. Working hours, the 30-minute grid, the closed day (20 Nov), the fixed clock (Mon 16 Nov 2026, 10:00 IST), one patient per slot, and cancellation by phone number are all in Rust. Message text cannot change those rules. `m07` says to ignore the rules and claims to be the clinic owner. The reading is only "book 02:00", and the code refuses it because the clinic is closed then.

## Double booking

The store keeps one SQLite connection behind a mutex. A request holds that lock while it checks the slot and inserts the row. A unique index on doctor and start, for rows that are still booked, rejects a second insert. `cargo test` sends eight parallel `POST /bookings` for the same slot and expects one 201 and seven 409s. The replay sends `m09` and `m10` together. Exactly one of them is booked.

## When the system asks

It asks when booking would mean guessing a missing or ambiguous field. `m14` says "11/12 at 5pm". Both numbers are 12 or under, so the date can be 11 December or 12 November. 12 November is already past. Booking 11 December would be a guess about the day. The reply is: "Which date do you mean for 5 PM?"

## What went wrong

The replay does not create a wrong booking or a double booking. The failure is the list of alternatives. When `m10` loses Wednesday noon with Dr Rao, the four offered times are mostly Dr Iyer. Dr Rao at 12:30 is the same distance as one of the Iyer slots and is cut off, because the ranking keeps the first four by clock distance and treats both doctors as equal. The patient asked for Rao.

## Another day

Call a model, at most three times, and reject broken JSON, unknown doctors, and impossible dates before falling back to this mock. When a slot is taken, offer the requested doctor's nearby times first. The clinic may also want a rule for `m09` and `m10` other than "whichever insert lands first".
