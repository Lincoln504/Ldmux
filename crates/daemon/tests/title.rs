//! `ldmux title`: naming a shell after the conversation in it.
//!
//! The rule is the daemon's, not the CLI's, so it is tested here through the same `handle` the CLI
//! calls: a title is one to three words, a shell must exist and be running, and the name the list
//! draws is the one that was set. The agent-side half (a pi or Claude Code session rename) is
//! covered by the unit tests in `title.rs`, where a temporary transcript stands in for the real one.

mod common;

use common::rig::Rig;

#[tokio::test]
async fn a_title_names_the_row_instead_of_the_folder() {
    let rig = Rig::start().await;
    let before = rpc(&rig, "list", serde_json::json!({})).await;
    let title_before = row_title(&before, &rig.session);
    assert_ne!(title_before, "Radio paths");

    let answer = rpc(
        &rig,
        "title",
        serde_json::json!({"session": rig.session, "title": "Radio paths"}),
    )
    .await;
    assert_eq!(answer["ok"], serde_json::Value::Bool(true), "{answer}");
    assert_eq!(answer["title"], "Radio paths");

    let after = rpc(&rig, "list", serde_json::json!({})).await;
    assert_eq!(
        row_title(&after, &rig.session),
        "Radio paths",
        "the list draws the conversation's name, not the directory's"
    );
}

#[tokio::test]
async fn a_second_title_replaces_the_first() {
    let rig = Rig::start().await;
    for name in ["First name", "Second"] {
        let answer = rpc(
            &rig,
            "title",
            serde_json::json!({"session": rig.session, "title": name}),
        )
        .await;
        assert_eq!(answer["ok"], serde_json::Value::Bool(true), "{answer}");
    }
    let list = rpc(&rig, "list", serde_json::json!({})).await;
    assert_eq!(row_title(&list, &rig.session), "Second");
}

#[tokio::test]
async fn four_words_are_refused_with_a_sentence() {
    let rig = Rig::start().await;
    let answer = rpc(
        &rig,
        "title",
        serde_json::json!({"session": rig.session, "title": "one two three four"}),
    )
    .await;
    assert_eq!(answer["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        answer["detail"],
        serde_json::Value::String("that is 4 words; a title is 3 at most".into())
    );
    // And the row is untouched.
    let list = rpc(&rig, "list", serde_json::json!({})).await;
    assert_ne!(row_title(&list, &rig.session), "one two three four");
}

#[tokio::test]
async fn a_shell_that_does_not_exist_is_refused() {
    let rig = Rig::start().await;
    let answer = rpc(
        &rig,
        "title",
        serde_json::json!({"session": "nosuchshell", "title": "Radio paths"}),
    )
    .await;
    assert_eq!(answer["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        answer["detail"],
        serde_json::Value::String("no such shell".into())
    );
}

/// The daemon's own answer, on the same `handle` the CLI calls.
async fn rpc(rig: &Rig, method: &str, params: serde_json::Value) -> serde_json::Value {
    rig.arc.lock().await.handle(method, params).await.unwrap()
}

/// The title the list draws for `id`, or an empty string when the row is missing.
fn row_title(list: &serde_json::Value, id: &str) -> String {
    list["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["id"].as_str() == Some(id))
        .and_then(|s| s["title"].as_str())
        .unwrap_or("")
        .to_string()
}
