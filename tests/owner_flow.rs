//! Black-box owner journey (`docs/architecture-internals.md` §9): register → discover a vet → add a
//! pet → check free slots → book → list "mine" → view pet detail → cancel.
//! Driven entirely over real HTTP against a spawned instance, mirroring
//! what a Playwright E2E suite would exercise through a UI.

mod support;

use serde_json::json;
use support::TestServer;

#[tokio::test]
async fn owner_can_book_and_then_cancel_an_appointment() {
    let server = TestServer::spawn().await;
    let client = server.client();

    let vet_token = server
        .register_and_login("vet@example.com", json!({"type": "VET"}))
        .await;
    let owner_token = server
        .register_and_login("owner@example.com", json!({"type": "OWNER"}))
        .await;

    // Vet sets a Monday 9:00-17:00 weekly schedule.
    let set_availability = client
        .post(server.url("/api/vets/availability"))
        .bearer_auth(&vet_token)
        .json(&json!({"slots": [
            {"day_of_week": "monday", "start_time": "09:00:00.0", "end_time": "17:00:00.0"},
        ]}))
        .send()
        .await
        .unwrap();
    assert_eq!(set_availability.status(), 200);

    // Owner discovers the vet through the directory — the only way to
    // learn a vet's id via the API.
    let vets: serde_json::Value = client
        .get(server.url("/api/vets"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let vets = vets.as_array().unwrap();
    assert_eq!(vets.len(), 1);
    // Registration no longer collects a specialty/name (the target wire
    // contract's `RegisterRequest` only carries `username`/`password`/
    // `type`), so the vet's directory `name` now defaults to `username`.
    assert_eq!(vets[0]["name"], "vet@example.com");
    let vet_id = vets[0]["id"].as_str().unwrap();

    // Owner adds a pet.
    let pet: serde_json::Value = client
        .post(server.url("/api/pets"))
        .bearer_auth(&owner_token)
        .json(&json!({
            "name": "Rex",
            "species": "DOG",
            "breed": "Labrador",
            "gender": "MALE",
            "birthDate": "2020-01-15",
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

    // Free slots for that Monday are the whole 9-17 window before booking.
    let free: serde_json::Value = client
        .get(server.url(&format!("/api/vets/{vet_id}/slots?date=2026-09-07")))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let free = free.as_array().unwrap();
    assert_eq!(free.len(), 1);
    assert_eq!(free[0]["start"], "2026-09-07 09:00:00.0");
    assert_eq!(free[0]["end"], "2026-09-07 17:00:00.0");

    // Book.
    let book_response = client
        .post(server.url("/api/appointments"))
        .bearer_auth(&owner_token)
        .json(&json!({
            "pet_id": pet_id,
            "vet_id": vet_id,
            "time_slot": "2026-09-07 10:00:00.0",
            "duration_minutes": 30,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(book_response.status(), 201);
    let appointment: serde_json::Value = book_response.json().await.unwrap();
    assert_eq!(appointment["status"], "booked");
    let appointment_id = appointment["id"].as_str().unwrap();

    // The booked slot no longer shows up as free.
    let free_after: serde_json::Value = client
        .get(server.url(&format!("/api/vets/{vet_id}/slots?date=2026-09-07")))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let free_after = free_after.as_array().unwrap();
    assert_eq!(
        free_after.len(),
        2,
        "booking should split the window in two"
    );

    // "mine" lists it.
    let mine: serde_json::Value = client
        .get(server.url("/api/appointments"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mine.as_array().unwrap().len(), 1);

    // Pet detail is reachable by its wire id.
    let pet_detail: serde_json::Value = client
        .get(server.url(&format!("/api/pets/{pet_id}")))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pet_detail["name"], "Rex");

    // Cancel — the booking is 4+ days out, well past the 24h cutoff.
    let cancel_response = client
        .post(server.url(&format!("/api/appointments/{appointment_id}/cancel")))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(cancel_response.status(), 200);
    let cancelled: serde_json::Value = cancel_response.json().await.unwrap();
    assert_eq!(cancelled["status"], "cancelled");
}

#[tokio::test]
async fn an_unauthenticated_request_gets_the_crates_json_error_shape() {
    let server = TestServer::spawn().await;

    let response = server
        .client()
        .get(server.url("/api/pets"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 401);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["code"], "UNAUTHENTICATED");
}
