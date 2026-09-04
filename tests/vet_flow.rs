//! Black-box vet journey (`docs/architecture-internals.md` §9): set availability → see a booking
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
        .register_and_login(
            "vet@example.com",
            json!({"role": "vet", "specialty": "Dentistry"}),
        )
        .await;
    let owner_token = server
        .register_and_login(
            "owner@example.com",
            json!({"role": "owner", "phone": "555-0100"}),
        )
        .await;

    client
        .post(server.url("/api/vets/availability"))
        .bearer_auth(&vet_token)
        .json(&json!({"slots": [
            {"day_of_week": "monday", "start_time": "09:00:00.0", "end_time": "17:00:00.0"},
        ]}))
        .send()
        .await
        .unwrap();

    let vets: serde_json::Value = client
        .get(server.url("/api/vets"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let vet_id = vets[0]["id"].as_str().unwrap().to_string();

    let pet: serde_json::Value = client
        .post(server.url("/api/pets"))
        .bearer_auth(&owner_token)
        .json(&json!({
            "name": "Whiskers",
            "type": "cat",
            "breed": "Siamese",
            "birth_date": "2019-06-01",
            "picture": "aGVsbG8=",
            "pictureContentType": "image/jpeg",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pet_id = pet["id"].as_str().unwrap();

    let booked: serde_json::Value = client
        .post(server.url("/api/appointments"))
        .bearer_auth(&owner_token)
        .json(&json!({
            "pet_id": pet_id,
            "vet_id": vet_id,
            "time_slot": "2026-09-07 11:00:00.0",
            "duration_minutes": 30,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let appointment_id = booked["id"].as_str().unwrap();

    // The vet's own calendar shows it.
    let calendar: serde_json::Value = client
        .get(server.url("/api/appointments"))
        .bearer_auth(&vet_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(calendar.as_array().unwrap().len(), 1);

    // A booked appointment can't be no-showed directly.
    let premature_no_show = client
        .post(server.url(&format!("/api/appointments/{appointment_id}/no-show")))
        .bearer_auth(&vet_token)
        .send()
        .await
        .unwrap();
    assert_eq!(premature_no_show.status(), 409);

    let confirm = client
        .post(server.url(&format!("/api/appointments/{appointment_id}/confirm")))
        .bearer_auth(&vet_token)
        .send()
        .await
        .unwrap();
    assert_eq!(confirm.status(), 200);
    let confirmed: serde_json::Value = confirm.json().await.unwrap();
    assert_eq!(confirmed["status"], "confirmed");

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

    // The owner sees it both via the dedicated endpoint and embedded in the
    // pet detail view (`docs/architecture-internals.md` §6).
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
    assert_eq!(pet_detail["visits"].as_array().unwrap().len(), 1);
}
