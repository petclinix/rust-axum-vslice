//! Black-box vet journey (`docs/architecture-internals.md` §9): create a location → see a booking
//! land on the calendar → confirm → complete → record a visit → duplicate
//! visit rejected → the visit shows up for the owner too.

mod support;

use serde_json::json;
use support::TestServer;

#[tokio::test]
async fn vet_can_run_an_appointment_through_to_a_recorded_visit() {
    let server = TestServer::spawn().await;
    let client = server.client();

    let vet_token = server
        .register_and_login("vet@example.com", json!({"type": "VET"}))
        .await;
    let owner_token = server
        .register_and_login("owner@example.com", json!({"type": "OWNER"}))
        .await;

    let location: serde_json::Value = client
        .post(server.url("/api/locations"))
        .bearer_auth(&vet_token)
        .json(&json!({
            "name": "Downtown Clinic",
            "zoneId": "Europe/Vienna",
            "street": "Main St 1",
            "postalCode": "1010",
            "city": "Vienna",
            "country": "Austria",
            "weeklyPeriods": [
                {"dayOfWeek": 1, "startTime": "09:00:00.0", "endTime": "17:00:00.0"},
            ],
            "overrides": [],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let location_id = location["id"].as_i64().unwrap();

    let pet: serde_json::Value = client
        .post(server.url("/api/pets"))
        .bearer_auth(&owner_token)
        .json(&json!({
            "name": "Whiskers",
            "species": "CAT",
            "breed": "Siamese",
            "gender": "FEMALE",
            "birthDate": "2019-06-01",
            "picture": "aGVsbG8=",
            "pictureContentType": "image/jpeg",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pet_id = pet["id"].as_i64().unwrap();

    let booked: serde_json::Value = client
        .post(server.url("/api/owner/appointments"))
        .bearer_auth(&owner_token)
        .json(&json!({
            "locationId": location_id,
            "petId": pet_id,
            "startsAt": "2026-09-07 11:00:00.0",
            "appointmentType": "CHECKUP",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let appointment_id = booked["id"].as_i64().unwrap();

    // The vet's own calendar shows it, with the pet name and owner
    // username joined in.
    let calendar: serde_json::Value = client
        .get(server.url("/api/vet/appointments"))
        .bearer_auth(&vet_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let calendar = calendar.as_array().unwrap();
    assert_eq!(calendar.len(), 1);
    assert_eq!(calendar[0]["petName"], "Whiskers");
    assert_eq!(calendar[0]["ownerUsername"], "owner@example.com");

    // A booked appointment can't be no-showed directly.
    let premature_no_show = client
        .put(server.url(&format!("/api/vet/appointments/{appointment_id}/no-show")))
        .bearer_auth(&vet_token)
        .send()
        .await
        .unwrap();
    assert_eq!(premature_no_show.status(), 409);

    // Confirm/no-show return `200 OK` with no body per the target
    // contract, so the resulting status is checked via the calendar.
    let confirm = client
        .put(server.url(&format!("/api/vet/appointments/{appointment_id}/confirm")))
        .bearer_auth(&vet_token)
        .send()
        .await
        .unwrap();
    assert_eq!(confirm.status(), 200);

    let calendar: serde_json::Value = client
        .get(server.url("/api/vet/appointments"))
        .bearer_auth(&vet_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(calendar[0]["status"], "CONFIRMED");

    // `complete` stays at its old, unprefixed path for now — the target
    // contract has no such endpoint; completing an appointment moves into
    // the visit-write handler once `visits` itself is migrated.
    let complete = client
        .post(server.url(&format!("/api/appointments/{appointment_id}/complete")))
        .bearer_auth(&vet_token)
        .send()
        .await
        .unwrap();
    assert_eq!(complete.status(), 200);

    let visit = client
        .post(server.url(&format!("/api/appointments/{appointment_id}/visit")))
        .bearer_auth(&vet_token)
        .json(&json!({"type": "diagnosis", "remark": "Healthy, no concerns."}))
        .send()
        .await
        .unwrap();
    assert_eq!(visit.status(), 201);

    // Recording it again is a conflict — one visit per appointment.
    let duplicate_visit = client
        .post(server.url(&format!("/api/appointments/{appointment_id}/visit")))
        .bearer_auth(&vet_token)
        .json(&json!({"type": "note", "remark": "duplicate attempt"}))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate_visit.status(), 409);

    // The owner sees it via the dedicated visit-history endpoint.
    let owner_visits: serde_json::Value = client
        .get(server.url(&format!("/api/pets/{pet_id}/visits")))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(owner_visits.as_array().unwrap().len(), 1);
    assert_eq!(owner_visits[0]["remark"], "Healthy, no concerns.");

    let pet_detail: serde_json::Value = client
        .get(server.url(&format!("/api/pets/{pet_id}")))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pet_detail["name"], "Whiskers");
}
