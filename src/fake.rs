//! An in-memory stand-in for libtdjson that simulates a tiny Telegram account, so
//! the whole stack above the FFI layer can be tested without network access.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::auth::{authorize, AuthPrompter, TdlibParams};
use crate::client::Client;
use crate::tdjson::TdBackend;
use crate::telegram::Telegram;

pub struct FakeChat {
    pub id: i64,
    pub title: String,
    pub username: Option<String>,
    pub chat_type: Value,
    /// Newest first.
    pub messages: Vec<Value>,
}

#[derive(Default)]
pub struct World {
    pub logged_in: bool,
    pub code: String,
    pub password: Option<String>,
    /// In chat list order.
    pub chats: Vec<FakeChat>,
    /// Chats TDLib "knows": the first `known_chats` ones plus those resolved otherwise.
    pub known_chats: usize,
    pub known_extra: HashSet<i64>,
    pub users: HashMap<i64, Value>,
    pub undeletable: HashSet<i64>,
    pub self_only: HashSet<i64>,
    /// Methods that fail once with a flood-wait error.
    pub flood_once: HashSet<&'static str>,
    pub requests: Vec<Value>,
    pub deletions: Vec<(Vec<i64>, bool)>,
    pub started: bool,
}

impl World {
    fn is_known(&self, chat_id: i64) -> bool {
        self.known_extra.contains(&chat_id) || self.chats.iter().take(self.known_chats).any(|c| c.id == chat_id)
    }

    fn chat_json(chat: &FakeChat) -> Value {
        json!({ "@type": "chat", "id": chat.id, "title": chat.title, "type": chat.chat_type })
    }
}

pub fn text_message(id: i64, user_id: i64, text: &str) -> Value {
    json!({
        "@type": "message", "id": id, "date": 1_700_000_000 + id,
        "sender_id": { "@type": "messageSenderUser", "user_id": user_id },
        "content": { "@type": "messageText", "text": { "@type": "formattedText", "text": text, "entities": [] } }
    })
}

pub fn supergroup(id: i64, title: &str, messages: Vec<Value>) -> FakeChat {
    FakeChat {
        id,
        title: title.into(),
        username: None,
        chat_type: json!({ "@type": "chatTypeSupergroup", "supergroup_id": -id - 1_000_000_000_000, "is_channel": false }),
        messages,
    }
}

pub struct FakeTd {
    world: Mutex<World>,
    outbox: Mutex<VecDeque<String>>,
    available: Condvar,
}

impl FakeTd {
    pub fn new(world: World) -> Arc<Self> {
        Arc::new(Self {
            world: Mutex::new(world),
            outbox: Mutex::default(),
            available: Condvar::new(),
        })
    }

    pub fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.world.lock().unwrap()
    }

    fn push(&self, client_id: i32, mut value: Value) {
        value["@client_id"] = json!(client_id);
        self.outbox.lock().unwrap().push_back(value.to_string());
        self.available.notify_all();
    }

    fn auth_update(state: &str, extra: Value) -> Value {
        let mut state_obj = extra;
        state_obj["@type"] = json!(state);
        json!({ "@type": "updateAuthorizationState", "authorization_state": state_obj })
    }

    fn handle(world: &mut World, request: &Value) -> (Value, Vec<Value>) {
        let ok = json!({ "@type": "ok" });
        let error = |code: i32, message: &str| json!({ "@type": "error", "code": code, "message": message });
        let method = request["@type"].as_str().unwrap_or_default();
        if world.flood_once.remove(method) {
            return (error(429, "Too Many Requests: retry after 0"), vec![]);
        }
        match method {
            "getOption" => (json!({ "@type": "optionValueString", "value": "1.8.67" }), vec![]),
            "setTdlibParameters" => {
                let next = if world.logged_in {
                    "authorizationStateReady"
                } else {
                    "authorizationStateWaitPhoneNumber"
                };
                (ok, vec![Self::auth_update(next, json!({}))])
            }
            "setAuthenticationPhoneNumber" => {
                if request["phone_number"].as_str().unwrap_or_default().starts_with('+') {
                    let info = json!({ "code_info": { "type": { "@type": "authenticationCodeTypeTelegramMessage", "length": 5 } } });
                    (ok, vec![Self::auth_update("authorizationStateWaitCode", info)])
                } else {
                    (error(400, "PHONE_NUMBER_INVALID"), vec![])
                }
            }
            "checkAuthenticationCode" => {
                if request["code"] != json!(world.code) {
                    (error(400, "PHONE_CODE_INVALID"), vec![])
                } else if world.password.is_some() {
                    (
                        ok,
                        vec![Self::auth_update(
                            "authorizationStateWaitPassword",
                            json!({ "password_hint": "pet" }),
                        )],
                    )
                } else {
                    world.logged_in = true;
                    (ok, vec![Self::auth_update("authorizationStateReady", json!({}))])
                }
            }
            "checkAuthenticationPassword" => {
                if request["password"].as_str() == world.password.as_deref() {
                    world.logged_in = true;
                    (ok, vec![Self::auth_update("authorizationStateReady", json!({}))])
                } else {
                    (error(400, "PASSWORD_HASH_INVALID"), vec![])
                }
            }
            "logOut" => {
                world.logged_in = false;
                let updates = ["authorizationStateLoggingOut", "authorizationStateClosed"];
                (ok, updates.iter().map(|s| Self::auth_update(s, json!({}))).collect())
            }
            "close" => {
                let updates = ["authorizationStateClosing", "authorizationStateClosed"];
                (ok, updates.iter().map(|s| Self::auth_update(s, json!({}))).collect())
            }
            "getMe" => (
                json!({ "@type": "user", "id": 1, "first_name": "Me", "last_name": "" }),
                vec![],
            ),
            "getUser" => match world.users.get(&request["user_id"].as_i64().unwrap_or_default()) {
                Some(user) => (user.clone(), vec![]),
                None => (error(404, "Not Found"), vec![]),
            },
            "loadChats" => {
                if request["chat_list"]["@type"] == "chatListMain" && world.known_chats < world.chats.len() {
                    world.known_chats = (world.known_chats + 2).min(world.chats.len());
                    (ok, vec![])
                } else {
                    (error(404, "Not Found"), vec![])
                }
            }
            "getChats" => {
                let limit = request["limit"].as_u64().unwrap_or(0) as usize;
                let ids: Vec<i64> = world
                    .chats
                    .iter()
                    .take(world.known_chats.min(limit))
                    .map(|c| c.id)
                    .collect();
                (
                    json!({ "@type": "chats", "total_count": world.chats.len(), "chat_ids": ids }),
                    vec![],
                )
            }
            "getChat" => {
                let id = request["chat_id"].as_i64().unwrap_or_default();
                match world.chats.iter().find(|c| c.id == id) {
                    Some(chat) if world.is_known(id) => (World::chat_json(chat), vec![]),
                    _ => (error(400, "Chat not found"), vec![]),
                }
            }
            "searchPublicChat" => {
                let username = request["username"].as_str().unwrap_or_default();
                match world.chats.iter().find(|c| c.username.as_deref() == Some(username)) {
                    Some(chat) => {
                        let value = World::chat_json(chat);
                        world.known_extra.insert(chat.id);
                        (value, vec![])
                    }
                    None => (error(400, "USERNAME_NOT_OCCUPIED"), vec![]),
                }
            }
            "searchChats" => {
                let query = request["query"].as_str().unwrap_or_default().to_lowercase();
                let ids: Vec<i64> = world
                    .chats
                    .iter()
                    .filter(|c| world.is_known(c.id) && c.title.to_lowercase().contains(&query))
                    .map(|c| c.id)
                    .collect();
                (
                    json!({ "@type": "chats", "total_count": ids.len(), "chat_ids": ids }),
                    vec![],
                )
            }
            "getChatHistory" => {
                let id = request["chat_id"].as_i64().unwrap_or_default();
                let from = request["from_message_id"].as_i64().unwrap_or(0);
                let Some(chat) = world.chats.iter().find(|c| c.id == id) else {
                    return (error(400, "Chat not found"), vec![]);
                };
                // Like TDLib, the first page only holds the last message, and later
                // pages are smaller than requested.
                let size = if from == 0 { 1 } else { 3 };
                let page: Vec<Value> = chat
                    .messages
                    .iter()
                    .filter(|m| from == 0 || m["id"].as_i64().unwrap() < from)
                    .take(size)
                    .cloned()
                    .collect();
                (
                    json!({ "@type": "messages", "total_count": page.len(), "messages": page }),
                    vec![],
                )
            }
            "searchChatMessages" => {
                let id = request["chat_id"].as_i64().unwrap_or_default();
                let query = request["query"].as_str().unwrap_or_default().to_lowercase();
                let from = request["from_message_id"].as_i64().unwrap_or(0);
                let Some(chat) = world.chats.iter().find(|c| c.id == id) else {
                    return (error(400, "Chat not found"), vec![]);
                };
                let hits: Vec<&Value> = chat
                    .messages
                    .iter()
                    .filter(|m| from == 0 || m["id"].as_i64().unwrap() < from)
                    .filter(|m| {
                        m["content"]["text"]["text"]
                            .as_str()
                            .unwrap_or_default()
                            .to_lowercase()
                            .contains(&query)
                    })
                    .collect();
                let page: Vec<Value> = hits.iter().take(2).map(|m| (*m).clone()).collect();
                let next = if hits.len() > 2 {
                    page.last().unwrap()["id"].as_i64().unwrap()
                } else {
                    0
                };
                (
                    json!({ "@type": "foundChatMessages", "total_count": hits.len(), "messages": page, "next_from_message_id": next }),
                    vec![],
                )
            }
            "getMessageProperties" => {
                let id = request["message_id"].as_i64().unwrap_or_default();
                let (for_self, for_everyone) = if world.undeletable.contains(&id) {
                    (false, false)
                } else if world.self_only.contains(&id) {
                    (true, false)
                } else {
                    (false, true)
                };
                (
                    json!({ "@type": "messageProperties", "can_be_deleted_only_for_self": for_self, "can_be_deleted_for_all_users": for_everyone }),
                    vec![],
                )
            }
            "deleteMessages" => {
                let chat_id = request["chat_id"].as_i64().unwrap_or_default();
                let ids: Vec<i64> = request["message_ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_i64)
                    .collect();
                if ids.iter().any(|id| world.undeletable.contains(id)) {
                    return (error(400, "Message can't be deleted"), vec![]);
                }
                if let Some(chat) = world.chats.iter_mut().find(|c| c.id == chat_id) {
                    chat.messages.retain(|m| !ids.contains(&m["id"].as_i64().unwrap()));
                }
                world
                    .deletions
                    .push((ids, request["revoke"].as_bool().unwrap_or(false)));
                (ok, vec![])
            }
            _ => (error(400, &format!("Unknown method {method}")), vec![]),
        }
    }
}

impl TdBackend for FakeTd {
    fn create_client_id(&self) -> i32 {
        1
    }

    fn send(&self, client_id: i32, request: &str) {
        let request: Value = serde_json::from_str(request).expect("requests are valid JSON");
        let (mut response, updates) = {
            let mut world = self.world.lock().unwrap();
            if !world.started {
                world.started = true;
                self.push(
                    client_id,
                    Self::auth_update("authorizationStateWaitTdlibParameters", json!({})),
                );
            }
            world.requests.push(request.clone());
            Self::handle(&mut world, &request)
        };
        if let Some(extra) = request.get("@extra") {
            response["@extra"] = extra.clone();
        }
        self.push(client_id, response);
        for update in updates {
            self.push(client_id, update);
        }
    }

    fn receive(&self, timeout: f64) -> Option<String> {
        let outbox = self.outbox.lock().unwrap();
        let (mut outbox, _) = self
            .available
            .wait_timeout_while(outbox, Duration::from_secs_f64(timeout), |queue| queue.is_empty())
            .unwrap();
        outbox.pop_front()
    }

    fn execute(&self, _request: &str) -> Option<String> {
        Some(json!({ "@type": "ok" }).to_string())
    }
}

/// Answers login prompts from a script and records what happened.
#[derive(Default)]
pub struct ScriptedPrompter {
    pub answers: VecDeque<String>,
    pub retries: Vec<String>,
}

impl ScriptedPrompter {
    pub fn new(answers: &[&str]) -> Self {
        Self {
            answers: answers.iter().map(|s| s.to_string()).collect(),
            retries: Vec::new(),
        }
    }

    fn next(&mut self) -> anyhow::Result<String> {
        self.answers
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no scripted answer left"))
    }
}

impl AuthPrompter for ScriptedPrompter {
    fn phone_number(&mut self) -> anyhow::Result<String> {
        self.next()
    }
    fn code(&mut self, _: &Value) -> anyhow::Result<String> {
        self.next()
    }
    fn email_address(&mut self) -> anyhow::Result<String> {
        self.next()
    }
    fn email_code(&mut self, _: &Value) -> anyhow::Result<String> {
        self.next()
    }
    fn password(&mut self, _: &str) -> anyhow::Result<String> {
        self.next()
    }
    fn retry(&mut self, message: &str) {
        self.retries.push(message.to_owned());
    }
    fn notice(&mut self, _: &str) {}
}

pub fn params() -> TdlibParams {
    TdlibParams {
        api_id: 1,
        api_hash: "0".repeat(32),
        database_directory: "/nonexistent".into(),
        use_test_dc: true,
    }
}

/// A logged-in `Telegram` backed by `world`.
pub fn logged_in(mut world: World) -> (Telegram, Arc<FakeTd>) {
    world.logged_in = true;
    let fake = FakeTd::new(world);
    let mut client = Client::new(fake.clone());
    client.set_request_timeout(Duration::from_secs(5));
    authorize(&client, &params(), &mut ScriptedPrompter::default()).unwrap();
    (Telegram::new(client), fake)
}
