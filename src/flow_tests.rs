//! End-to-end tests of login, chat resolution, scanning and deletion against the
//! in-memory fake TDLib.

use std::time::Duration;

use serde_json::json;

use crate::auth::authorize;
use crate::cleaner::{execute_plan, plan_deletion, scan, DeletionPlan, NoProgress, ScanMode, ScanOptions};
use crate::client::Client;
use crate::fake::{logged_in, params, supergroup, text_message, FakeChat, FakeTd, ScriptedPrompter, World};
use crate::matcher::{MatchOptions, Matcher};
use crate::message::Deletability;
use crate::telegram::{ChatKind, ChatRef};

const CHAT: i64 = -1001234567890;

fn spam_chat() -> FakeChat {
    supergroup(
        CHAT,
        "Crypto Talk",
        vec![
            text_message(10, 7, "Free AIRDROP, join t.me/scam"),
            text_message(9, 8, "good morning"),
            text_message(8, 7, "加微信 赚钱"),
            text_message(7, 8, "what's an airdrop?"),
            text_message(6, 8, "lunch?"),
            json!({ "@type": "message", "id": 5, "date": 0, "content": { "@type": "messageChatAddMembers" } }),
            text_message(4, 7, "AirDrop claim now"),
            text_message(3, 8, "bye"),
            text_message(2, 7, "加微信"),
            text_message(1, 8, "hello"),
        ],
    )
}

fn world_with(chats: Vec<FakeChat>) -> World {
    let known_chats = chats.len();
    World {
        chats,
        known_chats,
        ..Default::default()
    }
}

fn matcher(keywords: &[&str], options: MatchOptions) -> Matcher {
    Matcher::new(keywords.iter().map(|k| k.to_string()).collect(), options).unwrap()
}

fn ids(outcome: &crate::cleaner::ScanOutcome) -> Vec<i64> {
    outcome.matches.iter().map(|m| m.message.id).collect()
}

#[test]
fn login_retries_wrong_code_and_asks_for_password() {
    let fake = FakeTd::new(World {
        code: "22222".into(),
        password: Some("secret".into()),
        ..Default::default()
    });
    let mut client = Client::new(fake.clone());
    client.set_request_timeout(Duration::from_secs(5));
    let mut prompter = ScriptedPrompter::new(&["12345", "+99966", "11111", "22222", "wrong", "secret"]);
    authorize(&client, &params(), &mut prompter).unwrap();

    assert!(fake.world().logged_in);
    assert!(prompter.answers.is_empty());
    assert_eq!(prompter.retries.len(), 3, "{:?}", prompter.retries);
    assert!(prompter.retries[0].contains("PHONE_NUMBER_INVALID"));
    assert!(prompter.retries[1].contains("wrong code"));
    assert!(prompter.retries[2].contains("wrong password"));
    let parameters = fake
        .world()
        .requests
        .iter()
        .find(|r| r["@type"] == "setTdlibParameters")
        .cloned()
        .unwrap();
    assert_eq!(parameters["api_id"], 1);
    assert_eq!(parameters["use_message_database"], true);
}

#[test]
fn login_gives_up_after_too_many_wrong_codes() {
    let fake = FakeTd::new(World {
        code: "22222".into(),
        ..Default::default()
    });
    let client = Client::new(fake);
    let mut prompter = ScriptedPrompter::new(&["+1", "1", "2", "3", "4", "5"]);
    let err = authorize(&client, &params(), &mut prompter).unwrap_err();
    assert!(err.to_string().contains("login failed"), "{err}");
}

#[test]
fn full_scan_reads_every_page() {
    let (tg, _) = logged_in(world_with(vec![spam_chat()]));
    let m = matcher(&["airdrop", "加微信"], MatchOptions::default());
    let outcome = scan(
        &tg,
        CHAT,
        &m,
        &ScanOptions {
            mode: ScanMode::Full,
            limit: None,
        },
        &NoProgress,
    )
    .unwrap();
    assert_eq!(outcome.scanned, 10);
    assert_eq!(ids(&outcome), [10, 8, 7, 4, 2]);
    assert_eq!(outcome.matches[1].keywords, ["加微信"]);
    assert!(!outcome.interrupted);
    assert_eq!(
        outcome.matches[0].deletability,
        Some(Deletability {
            for_self: false,
            for_everyone: true
        })
    );
}

#[test]
fn full_scan_respects_limit_and_options() {
    let (tg, _) = logged_in(world_with(vec![spam_chat()]));
    let m = matcher(&["airdrop"], MatchOptions::default());
    let outcome = scan(
        &tg,
        CHAT,
        &m,
        &ScanOptions {
            mode: ScanMode::Full,
            limit: Some(4),
        },
        &NoProgress,
    )
    .unwrap();
    assert_eq!(outcome.scanned, 4);
    assert_eq!(ids(&outcome), [10, 7]);

    let m = matcher(
        &["AirDrop"],
        MatchOptions {
            case_sensitive: true,
            ..Default::default()
        },
    );
    let outcome = scan(
        &tg,
        CHAT,
        &m,
        &ScanOptions {
            mode: ScanMode::Full,
            limit: None,
        },
        &NoProgress,
    )
    .unwrap();
    assert_eq!(ids(&outcome), [4]);

    let m = matcher(
        &[r"t\.me/\w+"],
        MatchOptions {
            regex: true,
            ..Default::default()
        },
    );
    let outcome = scan(
        &tg,
        CHAT,
        &m,
        &ScanOptions {
            mode: ScanMode::Full,
            limit: None,
        },
        &NoProgress,
    )
    .unwrap();
    assert_eq!(ids(&outcome), [10]);
}

#[test]
fn search_mode_pages_through_results_and_rechecks_locally() {
    let (tg, fake) = logged_in(world_with(vec![spam_chat()]));
    fake.world().flood_once.insert("searchChatMessages");
    let m = matcher(&["airdrop", "加微信"], MatchOptions::default());
    let outcome = scan(
        &tg,
        CHAT,
        &m,
        &ScanOptions {
            mode: ScanMode::Search,
            limit: None,
        },
        &NoProgress,
    )
    .unwrap();
    assert_eq!(ids(&outcome), [10, 8, 7, 4, 2]);
    // Only the matching messages were examined.
    assert_eq!(outcome.scanned, 5);

    // The server search ignores case; the local check does not.
    let m = matcher(
        &["AirDrop"],
        MatchOptions {
            case_sensitive: true,
            ..Default::default()
        },
    );
    let outcome = scan(
        &tg,
        CHAT,
        &m,
        &ScanOptions {
            mode: ScanMode::Search,
            limit: None,
        },
        &NoProgress,
    )
    .unwrap();
    assert_eq!(ids(&outcome), [4]);

    let m = matcher(
        &["x"],
        MatchOptions {
            regex: true,
            ..Default::default()
        },
    );
    assert!(scan(
        &tg,
        CHAT,
        &m,
        &ScanOptions {
            mode: ScanMode::Search,
            limit: None
        },
        &NoProgress
    )
    .is_err());
}

#[test]
fn deletion_plan_respects_permissions() {
    let (tg, fake) = logged_in(world_with(vec![spam_chat()]));
    {
        let mut world = fake.world();
        world.undeletable.insert(8);
        world.self_only.insert(7);
    }
    let m = matcher(&["airdrop", "加微信"], MatchOptions::default());
    let outcome = scan(
        &tg,
        CHAT,
        &m,
        &ScanOptions {
            mode: ScanMode::Full,
            limit: None,
        },
        &NoProgress,
    )
    .unwrap();

    let plan = plan_deletion(&outcome.matches, true);
    assert_eq!(plan.for_everyone, [10, 4, 2]);
    assert_eq!(plan.for_self, [7]);
    assert_eq!(plan.skipped, [(8, "no permission to delete it")]);

    let plan = plan_deletion(&outcome.matches, false);
    assert_eq!(plan.for_everyone, Vec::<i64>::new());
    assert_eq!(plan.for_self, [7]);
    assert_eq!(plan.skipped.len(), 4);
}

#[test]
fn deletes_in_batches_and_isolates_failures() {
    let (tg, fake) = logged_in(world_with(vec![spam_chat()]));
    {
        let mut world = fake.world();
        world.undeletable.insert(4);
        world.flood_once.insert("deleteMessages");
    }
    // Deletability unknown for every message, so all are attempted.
    let plan = DeletionPlan {
        for_everyone: vec![10, 8, 4],
        for_self: vec![2],
        skipped: vec![],
    };
    let report = execute_plan(&tg, CHAT, &plan, &NoProgress).unwrap();
    assert_eq!(report.deleted_for_everyone, 2);
    assert_eq!(report.deleted_for_self, 1);
    assert_eq!(report.failed, [(4, "Message can't be deleted".to_owned())]);

    let world = fake.world();
    assert_eq!(world.deletions, [(vec![10], true), (vec![8], true), (vec![2], false)]);
    let remaining: Vec<i64> = world.chats[0]
        .messages
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    assert_eq!(remaining, [9, 7, 6, 5, 4, 3, 1]);
}

#[test]
fn large_plans_are_split_into_chunks_of_100() {
    let messages = (1..=250).rev().map(|id| text_message(id, 7, "spam")).collect();
    let (tg, fake) = logged_in(world_with(vec![supergroup(CHAT, "Big", messages)]));
    let plan = DeletionPlan {
        for_everyone: (1..=250).rev().collect(),
        ..Default::default()
    };
    let report = execute_plan(&tg, CHAT, &plan, &NoProgress).unwrap();
    assert_eq!(report.deleted(), 250);
    let sizes: Vec<usize> = fake.world().deletions.iter().map(|(ids, _)| ids.len()).collect();
    assert_eq!(sizes, [100, 100, 50]);
}

#[test]
fn resolves_chats_by_id_username_and_title() {
    let mut other = supergroup(-1009, "Crypto Talk Archive", vec![]);
    other.username = Some("archive_group".into());
    let private = FakeChat {
        id: 42,
        title: "Alice".into(),
        username: None,
        chat_type: json!({ "@type": "chatTypePrivate", "user_id": 42 }),
        messages: vec![],
    };
    let mut world = world_with(vec![spam_chat(), private, other]);
    // Only the first chat is known until the chat list is loaded.
    world.known_chats = 1;
    let (tg, _) = logged_in(world);

    let chat = tg.resolve_chat(&ChatRef::Id(42)).unwrap();
    assert_eq!((chat.title.as_str(), chat.kind), ("Alice", ChatKind::Private));
    let chat = tg.resolve_chat(&ChatRef::Username("archive_group".into())).unwrap();
    assert_eq!(chat.id, -1009);
    let chat = tg.resolve_chat(&ChatRef::Title("crypto talk".into())).unwrap();
    assert_eq!(chat.id, CHAT);

    let err = tg
        .resolve_chat(&ChatRef::Title("Crypto".into()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no chat is titled exactly"), "{err}");
    assert!(tg.resolve_chat(&ChatRef::Id(123)).is_err());
    assert!(tg.resolve_chat(&ChatRef::Title("nothing".into())).is_err());
    assert_eq!(tg.chats(10).unwrap().len(), 3);
}

#[test]
fn closing_waits_for_tdlib() {
    let (tg, fake) = logged_in(world_with(vec![]));
    drop(tg);
    assert!(fake.world().requests.iter().any(|r| r["@type"] == "close"));
}
