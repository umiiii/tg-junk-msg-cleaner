//! Typed wrappers around the handful of TDLib methods this tool uses.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::client::{Client, TdError};
use crate::message::{Deletability, SenderId};

/// TDLib's (and Telegram's) maximum page size for history, search and deletion.
pub const PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    Private,
    Secret,
    Group,
    Supergroup,
    Channel,
}

impl ChatKind {
    fn from_json(chat_type: &Value) -> Self {
        match chat_type["@type"].as_str().unwrap_or_default() {
            "chatTypeBasicGroup" => Self::Group,
            "chatTypeSupergroup" if chat_type["is_channel"].as_bool() == Some(true) => Self::Channel,
            "chatTypeSupergroup" => Self::Supergroup,
            "chatTypeSecret" => Self::Secret,
            _ => Self::Private,
        }
    }

    /// Whether deleting "for me only" is meaningful. In supergroups and channels
    /// every deletion is for everyone.
    pub fn supports_delete_for_self(self) -> bool {
        matches!(self, Self::Private | Self::Group)
    }
}

impl fmt::Display for ChatKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Private => "private",
            Self::Secret => "secret",
            Self::Group => "group",
            Self::Supergroup => "supergroup",
            Self::Channel => "channel",
        })
    }
}

#[derive(Debug, Clone)]
pub struct ChatInfo {
    pub id: i64,
    pub title: String,
    pub kind: ChatKind,
}

impl ChatInfo {
    fn from_json(chat: &Value) -> Result<Self, TdError> {
        Ok(Self {
            id: chat["id"]
                .as_i64()
                .ok_or_else(|| TdError::local("TDLib returned a chat without id"))?,
            title: chat["title"].as_str().unwrap_or_default().to_owned(),
            kind: ChatKind::from_json(&chat["type"]),
        })
    }
}

impl fmt::Display for ChatInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let title = if self.title.is_empty() {
            "(no title)"
        } else {
            &self.title
        };
        write!(f, "{title} [{}, id {}]", self.kind, self.id)
    }
}

/// How the user referred to a chat on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRef {
    Id(i64),
    Username(String),
    Title(String),
}

impl ChatRef {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            bail!("the chat must not be empty");
        }
        if let Ok(id) = input.parse::<i64>() {
            return Ok(Self::Id(id));
        }
        if let Some(username) = input.strip_prefix('@') {
            return Ok(Self::Username(username.to_owned()));
        }
        let lower = input.to_ascii_lowercase();
        let link = ["https://", "http://", ""].iter().find_map(|scheme| {
            ["t.me/", "telegram.me/", "telegram.dog/"].iter().find_map(|host| {
                lower
                    .strip_prefix(&format!("{scheme}{host}"))
                    .map(|_| scheme.len() + host.len())
            })
        });
        if let Some(prefix_len) = link {
            return Self::parse_link_path(&input[prefix_len..]);
        }
        Ok(Self::Title(input.to_owned()))
    }

    fn parse_link_path(path: &str) -> Result<Self> {
        let mut segments = path.split(['/', '?', '#']).filter(|s| !s.is_empty());
        match segments.next() {
            // https://t.me/c/1234567890/42 is a link into a private supergroup or channel.
            Some("c") => {
                let internal_id: i64 = segments
                    .next()
                    .and_then(|id| id.parse().ok())
                    .ok_or_else(|| anyhow!("unrecognised t.me/c/ link"))?;
                Ok(Self::Id(-1_000_000_000_000 - internal_id))
            }
            Some(invite) if invite.starts_with('+') || invite == "joinchat" => {
                bail!("invite links are not supported; join the chat first and pass its ID or title")
            }
            Some(username) => Ok(Self::Username(username.to_owned())),
            None => bail!("the link does not name a chat"),
        }
    }
}

pub struct UserSummary {
    pub id: i64,
    pub name: String,
}

pub struct Telegram {
    client: Client,
    names: RefCell<HashMap<SenderId, String>>,
}

impl Telegram {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            names: RefCell::default(),
        }
    }

    pub fn me(&self) -> Result<UserSummary, TdError> {
        let user = self.client.request(json!({ "@type": "getMe" }))?;
        Ok(UserSummary {
            id: user["id"].as_i64().unwrap_or_default(),
            name: user_display_name(&user),
        })
    }

    pub fn chat(&self, chat_id: i64) -> Result<ChatInfo, TdError> {
        ChatInfo::from_json(&self.client.request(json!({ "@type": "getChat", "chat_id": chat_id }))?)
    }

    /// Returns up to `limit` chats of the main chat list, in the order the
    /// official apps show them.
    pub fn chats(&self, limit: usize) -> Result<Vec<ChatInfo>> {
        let mut ids = self.chat_ids(limit)?;
        while ids.len() < limit && self.load_more_chats("chatListMain")? {
            ids = self.chat_ids(limit)?;
        }
        ids.into_iter().map(|id| self.chat(id).map_err(Into::into)).collect()
    }

    fn chat_ids(&self, limit: usize) -> Result<Vec<i64>, TdError> {
        let chats = self
            .client
            .request(json!({ "@type": "getChats", "chat_list": null, "limit": limit }))?;
        Ok(chats["chat_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect())
    }

    /// Loads another page of a chat list. Returns false once the list is complete.
    fn load_more_chats(&self, list: &str) -> Result<bool, TdError> {
        match self
            .client
            .request(json!({ "@type": "loadChats", "chat_list": { "@type": list }, "limit": PAGE_SIZE }))
        {
            Ok(_) => Ok(true),
            Err(err) if err.code == 404 => Ok(false),
            Err(err) => Err(err),
        }
    }

    pub fn resolve_chat(&self, reference: &ChatRef) -> Result<ChatInfo> {
        match reference {
            ChatRef::Id(id) => self.resolve_chat_id(*id),
            ChatRef::Username(username) => {
                let chat = self
                    .client
                    .request(json!({ "@type": "searchPublicChat", "username": username }))
                    .map_err(|err| anyhow!("could not find @{username}: {err}"))?;
                Ok(ChatInfo::from_json(&chat)?)
            }
            ChatRef::Title(title) => self.resolve_chat_title(title),
        }
    }

    fn resolve_chat_id(&self, id: i64) -> Result<ChatInfo> {
        if let Ok(chat) = self.chat(id) {
            return Ok(chat);
        }
        // TDLib only knows chats it has seen; load the chat lists and try again.
        for list in ["chatListMain", "chatListArchive"] {
            while self.load_more_chats(list)? {
                if let Ok(chat) = self.chat(id) {
                    return Ok(chat);
                }
            }
        }
        self.chat(id)
            .map_err(|err| anyhow!("chat {id} not found ({err}); run the `chats` command to list chat IDs"))
    }

    fn resolve_chat_title(&self, title: &str) -> Result<ChatInfo> {
        // searchChats only searches chats TDLib already knows about.
        for list in ["chatListMain", "chatListArchive"] {
            while self.load_more_chats(list)? {}
        }
        let found = self.client.request(json!({
            "@type": "searchChats", "query": title, "type_filter": null, "limit": 50,
        }))?;
        let candidates: Vec<ChatInfo> = found["chat_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .map(|id| self.chat(id))
            .collect::<Result<_, _>>()?;
        let wanted = title.to_lowercase();
        let exact: Vec<&ChatInfo> = candidates.iter().filter(|c| c.title.to_lowercase() == wanted).collect();
        // Only an exact (case-insensitive) title is accepted, so a typo can never
        // select some other chat for deletion.
        let describe = |chats: &mut dyn Iterator<Item = &ChatInfo>| -> String {
            chats.take(10).map(|c| format!("\n  {c}")).collect()
        };
        match exact.as_slice() {
            [chat] => Ok((*chat).clone()),
            [] if candidates.is_empty() => bail!("no chat titled {title:?}; run the `chats` command to list chat IDs"),
            [] => bail!(
                "no chat is titled exactly {title:?}; similar chats (pass the ID or the exact title):{}",
                describe(&mut candidates.iter())
            ),
            _ => bail!(
                "several chats are titled {title:?}; pass the chat ID instead:{}",
                describe(&mut exact.iter().copied())
            ),
        }
    }

    /// One page of chat history, newest first, strictly older than `from_message_id`
    /// (0 = start from the newest message).
    pub fn history(&self, chat_id: i64, from_message_id: i64) -> Result<Vec<Value>, TdError> {
        let result = self.client.request(json!({
            "@type": "getChatHistory",
            "chat_id": chat_id,
            "from_message_id": from_message_id,
            "offset": 0,
            "limit": PAGE_SIZE,
            "only_local": false,
        }))?;
        Ok(non_null_messages(result))
    }

    /// One page of server-side search results and the `from_message_id` of the
    /// next page (0 when there are no more results).
    pub fn search(&self, chat_id: i64, query: &str, from_message_id: i64) -> Result<(Vec<Value>, i64), TdError> {
        let mut result = self.client.request(json!({
            "@type": "searchChatMessages",
            "chat_id": chat_id,
            "topic_id": null,
            // TDLib versions before topic_id was introduced expect this field instead.
            "message_thread_id": 0,
            "query": query,
            "sender_id": null,
            "from_message_id": from_message_id,
            "offset": 0,
            "limit": PAGE_SIZE,
            "filter": null,
        }))?;
        let next = result["next_from_message_id"].as_i64().unwrap_or(0);
        Ok((non_null_messages(result.take()), next))
    }

    /// Asks TDLib how a message can be deleted (an offline call).
    pub fn deletability(&self, chat_id: i64, message_id: i64) -> Result<Deletability, TdError> {
        let properties = self.client.request(json!({
            "@type": "getMessageProperties", "chat_id": chat_id, "message_id": message_id,
        }))?;
        Deletability::from_json(&properties).ok_or_else(|| TdError::local("unexpected getMessageProperties result"))
    }

    pub fn delete_messages(&self, chat_id: i64, message_ids: &[i64], revoke: bool) -> Result<(), TdError> {
        self.client
            .request(json!({
                "@type": "deleteMessages", "chat_id": chat_id, "message_ids": message_ids, "revoke": revoke,
            }))
            .map(drop)
    }

    /// Display name of a message sender, cached.
    pub fn sender_name(&self, sender: SenderId) -> String {
        if let Some(name) = self.names.borrow().get(&sender) {
            return name.clone();
        }
        let name = match sender {
            SenderId::User(id) => self
                .client
                .request(json!({ "@type": "getUser", "user_id": id }))
                .map(|user| user_display_name(&user))
                .unwrap_or_else(|_| format!("user {id}")),
            SenderId::Chat(id) => self
                .chat(id)
                .map(|chat| chat.title)
                .unwrap_or_else(|_| format!("chat {id}")),
            SenderId::Unknown => "unknown sender".to_owned(),
        };
        self.names.borrow_mut().insert(sender, name.clone());
        name
    }

    pub fn log_out(&self) -> Result<(), TdError> {
        self.client.request(json!({ "@type": "logOut" })).map(drop)
    }
}

fn non_null_messages(mut result: Value) -> Vec<Value> {
    match result["messages"].take() {
        Value::Array(messages) => messages.into_iter().filter(|m| !m.is_null()).collect(),
        _ => Vec::new(),
    }
}

fn user_display_name(user: &Value) -> String {
    let first = user["first_name"].as_str().unwrap_or_default();
    let last = user["last_name"].as_str().unwrap_or_default();
    let mut name = format!("{first} {last}").trim().to_owned();
    if name.is_empty() {
        name = "Deleted Account".to_owned();
    }
    if let Some(username) = user["usernames"]["active_usernames"][0].as_str() {
        name.push_str(&format!(" (@{username})"));
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_references() {
        assert_eq!(ChatRef::parse("-1001234567890").unwrap(), ChatRef::Id(-1001234567890));
        assert_eq!(ChatRef::parse(" 777 ").unwrap(), ChatRef::Id(777));
        assert_eq!(
            ChatRef::parse("@spam_group").unwrap(),
            ChatRef::Username("spam_group".into())
        );
        assert_eq!(
            ChatRef::parse("https://t.me/spam_group").unwrap(),
            ChatRef::Username("spam_group".into())
        );
        assert_eq!(
            ChatRef::parse("t.me/Spam_Group/123").unwrap(),
            ChatRef::Username("Spam_Group".into())
        );
        assert_eq!(
            ChatRef::parse("https://t.me/c/1234567890/42").unwrap(),
            ChatRef::Id(-1001234567890)
        );
        assert_eq!(ChatRef::parse("My Group").unwrap(), ChatRef::Title("My Group".into()));
        assert!(ChatRef::parse("https://t.me/+AbCdEf").is_err());
        assert!(ChatRef::parse("https://t.me/joinchat/AbCdEf").is_err());
        assert!(ChatRef::parse("  ").is_err());
    }

    #[test]
    fn formats_user_names() {
        let user = json!({ "first_name": "Ann", "last_name": "", "usernames": { "active_usernames": ["ann"] } });
        assert_eq!(user_display_name(&user), "Ann (@ann)");
        assert_eq!(
            user_display_name(&json!({ "first_name": "", "last_name": "" })),
            "Deleted Account"
        );
    }

    #[test]
    fn classifies_chat_kinds() {
        assert_eq!(
            ChatKind::from_json(&json!({ "@type": "chatTypeSupergroup", "is_channel": true })),
            ChatKind::Channel
        );
        assert_eq!(
            ChatKind::from_json(&json!({ "@type": "chatTypeSupergroup", "is_channel": false })),
            ChatKind::Supergroup
        );
        assert_eq!(
            ChatKind::from_json(&json!({ "@type": "chatTypeBasicGroup" })),
            ChatKind::Group
        );
        assert_eq!(
            ChatKind::from_json(&json!({ "@type": "chatTypePrivate" })),
            ChatKind::Private
        );
    }
}
