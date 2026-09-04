//! Black-box owner journey (`docs/architecture-internals.md` §9): register → discover a vet →
//! create a pet → check free slots at their location → book → list "mine" →
//! view pet detail → cancel.
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

    // Vet creates a location with a Monday 9:00-17:00 weekly schedule.
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
    // `type`), so the vet's directory `username` is the registered one.
    assert_eq!(vets[0]["username"], "vet@example.com");

    // And through the owner-facing bookable-locations listing, which
    // carries the vet's username directly.
    let bookable: serde_json::Value = client
        .get(server.url("/api/owner/locations"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bookable = bookable.as_array().unwrap();
    assert_eq!(bookable.len(), 1);
    assert_eq!(bookable[0]["vetUsername"], "vet@example.com");

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
        .get(server.url(&format!(
            "/api/owner/locations/{location_id}/available-slots?date=2026-09-07&appointmentType=CHECKUP"
        )))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let free = free.as_array().unwrap();
    assert_eq!(free.len(), 1);
    assert_eq!(free[0]["startsAt"], "2026-09-07 09:00:00.0");
    assert_eq!(free[0]["endsAt"], "2026-09-07 17:00:00.0");

    // Book.
    let book_response = client
        .post(server.url("/api/owner/appointments"))
        .bearer_auth(&owner_token)
        .json(&json!({
            "locationId": location_id,
            "petId": pet_id,
            "startsAt": "2026-09-07 10:00:00.0",
            "appointmentType": "CHECKUP",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(book_response.status(), 201);
    let appointment: serde_json::Value = book_response.json().await.unwrap();
    assert_eq!(appointment["status"], "BOOKED");
    let appointment_id = appointment["id"].as_i64().unwrap();

    // The booked slot no longer shows up as free.
    let free_after: serde_json::Value = client
        .get(server.url(&format!(
            "/api/owner/locations/{location_id}/available-slots?date=2026-09-07&appointmentType=CHECKUP"
        )))
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
        .get(server.url("/api/owner/appointments"))
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

    // Cancel — the booking is 4+ days out, well past the 24h cutoff. The
    // target contract declares this endpoint `200 OK` with no content.
    let cancel_response = client
        .delete(server.url(&format!("/api/owner/appointments/{appointment_id}")))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(cancel_response.status(), 200);

    let mine_after_cancel: serde_json::Value = client
        .get(server.url("/api/owner/appointments"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mine_after_cancel[0]["status"], "CANCELLED");
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
