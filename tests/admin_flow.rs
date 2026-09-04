//! Black-box admin journey (`docs/architecture-internals.md` §9): the seeded admin account logs in
//! (no self-registration path exists for it — see `docs/architecture.md`'s
//! Auth Design section), lists users,
//! reads stats and the activity log other slices wrote to, then deactivates
//! a user (confirming it can no longer log in) and reactivates them.

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
    let user_id = target["id"].as_i64().unwrap();

    let stats: serde_json::Value = client
        .get(server.url("/api/admin/stats"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["totalPets"], 0);
    assert_eq!(stats["totalOwners"], 2);

    let activity: serde_json::Value = client
        .get(server.url("/api/admin/activity-logs"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries = activity.as_array().unwrap();
    let actions: Vec<&str> = entries
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(actions.contains(&"user_registered"));
    assert!(actions.contains(&"user_login"));
    assert!(
        entries
            .iter()
            .all(|e| e["id"].is_i64() && e["username"].is_string())
    );

    let deactivate = client
        .put(server.url(&format!("/api/admin/users/{user_id}/deactivate")))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(deactivate.status(), 200);
    let deactivated: serde_json::Value = deactivate.json().await.unwrap();
    assert_eq!(deactivated["active"], false);

    let blocked_login = client
        .post(server.url("/api/auth/login"))
        .json(&json!({"username": "toDeactivate@example.com", "password": "correct horse"}))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_login.status(), 403);
    let body: serde_json::Value = blocked_login.json().await.unwrap();
    assert_eq!(body["code"], "ACCOUNT_DEACTIVATED");

    let activate = client
        .put(server.url(&format!("/api/admin/users/{user_id}/activate")))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(activate.status(), 200);
    let activated: serde_json::Value = activate.json().await.unwrap();
    assert_eq!(activated["active"], true);

    let restored_login = client
        .post(server.url("/api/auth/login"))
        .json(&json!({"username": "toDeactivate@example.com", "password": "correct horse"}))
        .send()
        .await
        .unwrap();
    assert_eq!(restored_login.status(), 200);
}
