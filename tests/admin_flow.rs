//! Black-box admin journey (`docs/architecture-internals.md` §9): the seeded admin account logs in
//! (no self-registration path exists for it — see `docs/architecture.md`'s
//! Auth Design section), lists users,
//! reads stats and the activity log other slices wrote to, then deactivates
//! a user and confirms it can no longer log in.

mod support;

use serde_json::json;
use support::TestServer;

#[tokio::test]
async fn admin_can_manage_users_and_read_stats_and_activity() {
    let server = TestServer::spawn().await;
    let client = server.client();

    let admin_token = server.admin_token().await;

    let regular_owner_token = server
        .register_and_login("someoneelse@example.com", json!({"type": "OWNER"}))
        .await;

    // A non-admin gets 403 from every admin endpoint.
    let forbidden = client
        .get(server.url("/api/admin/users"))
        .bearer_auth(&regular_owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);

    // `register_and_login` already asserts login succeeds while the account
    // is still active — no need to keep the token around for that.
    server
        .register_and_login("toDeactivate@example.com", json!({"type": "OWNER"}))
        .await;

    let users: serde_json::Value = client
        .get(server.url("/api/admin/users"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let users = users.as_array().unwrap();
    assert_eq!(users.len(), 3, "seeded admin + two registered owners");
    let target = users
        .iter()
        .find(|u| u["username"] == "toDeactivate@example.com")
        .expect("registered owner should be listed");
    let user_id = target["id"].as_str().unwrap();

    let stats: serde_json::Value = client
        .get(server.url("/api/admin/stats"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["total_pets"], 0);

    let activity: serde_json::Value = client
        .get(server.url("/api/admin/activity"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let event_types: Vec<&str> = activity
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    assert!(event_types.contains(&"user_registered"));
    assert!(event_types.contains(&"user_login"));

    let deactivate = client
        .post(server.url(&format!("/api/admin/users/{user_id}/deactivate")))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(deactivate.status(), 200);
    let deactivated: serde_json::Value = deactivate.json().await.unwrap();
    assert_eq!(deactivated["is_active"], false);

    let blocked_login = client
        .post(server.url("/api/auth/login"))
        .json(&json!({"username": "toDeactivate@example.com", "password": "correct horse"}))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_login.status(), 403);
    let body: serde_json::Value = blocked_login.json().await.unwrap();
    assert_eq!(body["code"], "ACCOUNT_DEACTIVATED");
}
