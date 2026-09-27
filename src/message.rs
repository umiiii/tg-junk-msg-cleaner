//! Extracts the parts of TDLib `message` objects this tool cares about.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SenderId {
    User(i64),
    Chat(i64),
    Unknown,
}

impl SenderId {
    fn from_json(value: &Value) -> Self {
        match value["@type"].as_str() {
            Some("messageSenderUser") => value["user_id"].as_i64().map_or(Self::Unknown, Self::User),
            Some("messageSenderChat") => value["chat_id"].as_i64().map_or(Self::Unknown, Self::Chat),
            _ => Self::Unknown,
        }
    }
}

/// Whether a message may be deleted only for the current user and/or for everyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deletability {
    pub for_self: bool,
    pub for_everyone: bool,
}

impl Deletability {
    /// Reads `can_be_deleted_only_for_self` / `can_be_deleted_for_all_users` from a
    /// `messageProperties` object (or from a `message` object in TDLib < 1.8.4x).
    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            for_self: value["can_be_deleted_only_for_self"].as_bool()?,
            for_everyone: value["can_be_deleted_for_all_users"].as_bool()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct MessageInfo {
    pub id: i64,
    pub date: i64,
    pub sender: SenderId,
    /// All user-visible text of the message: text, captions, poll options, hidden link URLs, ...
    pub text: String,
    /// Present only when the TDLib version still reports it on `message`.
    pub deletability: Option<Deletability>,
}

impl MessageInfo {
    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            id: value["id"].as_i64()?,
            date: value["date"].as_i64().unwrap_or(0),
            sender: SenderId::from_json(&value["sender_id"]),
            text: searchable_text(&value["content"]),
            deletability: Deletability::from_json(value),
        })
    }
}

/// Collects the text a user can see in a `MessageContent` object.
pub fn searchable_text(content: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut add = |value: &Value| collect_formatted_text(value, &mut parts);

    match content["@type"].as_str().unwrap_or_default() {
        "messageText" => add(&content["text"]),
        "messagePoll" => {
            add(&content["poll"]["question"]);
            if let Some(options) = content["poll"]["options"].as_array() {
                options.iter().for_each(|option| add(&option["text"]));
            }
            add(&content["description"]);
        }
        "messageChecklist" => {
            add(&content["list"]["title"]);
            if let Some(tasks) = content["list"]["tasks"].as_array() {
                tasks.iter().for_each(|task| add(&task["text"]));
            }
        }
        "messageGame" => {
            add(&content["game"]["title"]);
            add(&content["game"]["description"]);
            add(&content["game"]["text"]);
        }
        "messageInvoice" => {
            add(&content["product_info"]["title"]);
            add(&content["product_info"]["description"]);
            add(&content["paid_media_caption"]);
        }
        "messageVenue" => {
            add(&content["venue"]["title"]);
            add(&content["venue"]["address"]);
        }
        "messageContact" => {
            add(&content["contact"]["first_name"]);
            add(&content["contact"]["last_name"]);
        }
        _ => {}
    }
    // Photos, videos, documents, audio, animations, voice notes, paid media, ...
    add(&content["caption"]);

    parts.join("\n")
}

/// Appends a `formattedText` (or a plain string, as older TDLib versions use in
/// some places) plus the URLs hidden behind text links, which spam often uses.
fn collect_formatted_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) if !text.is_empty() => parts.push(text.clone()),
        Value::Object(_) => {
            if let Some(text) = value["text"].as_str().filter(|text| !text.is_empty()) {
                parts.push(text.to_owned());
            }
            for entity in value["entities"].as_array().into_iter().flatten() {
                if let Some(url) = entity["type"]["url"].as_str() {
                    parts.push(url.to_owned());
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_plain_text() {
        let msg = json!({
            "@type": "message", "id": 42, "date": 1700000000,
            "sender_id": { "@type": "messageSenderUser", "user_id": 7 },
            "content": { "@type": "messageText", "text": { "@type": "formattedText", "text": "hello", "entities": [] } }
        });
        let info = MessageInfo::from_json(&msg).unwrap();
        assert_eq!(info.id, 42);
        assert_eq!(info.date, 1700000000);
        assert_eq!(info.sender, SenderId::User(7));
        assert_eq!(info.text, "hello");
        assert_eq!(info.deletability, None);
    }

    #[test]
    fn extracts_caption_and_hidden_links() {
        let content = json!({
            "@type": "messagePhoto",
            "caption": {
                "text": "click here",
                "entities": [{ "offset": 0, "length": 5, "type": { "@type": "textEntityTypeTextUrl", "url": "https://spam.example" } }]
            }
        });
        assert_eq!(searchable_text(&content), "click here\nhttps://spam.example");
    }

    #[test]
    fn extracts_poll_in_old_and_new_format() {
        let new = json!({
            "@type": "messagePoll",
            "poll": { "question": { "text": "Earn?" }, "options": [{ "text": { "text": "yes" } }, { "text": { "text": "no" } }] }
        });
        assert_eq!(searchable_text(&new), "Earn?\nyes\nno");
        let old = json!({
            "@type": "messagePoll",
            "poll": { "question": "Earn?", "options": [{ "text": "yes" }] }
        });
        assert_eq!(searchable_text(&old), "Earn?\nyes");
    }

    #[test]
    fn service_messages_have_no_text() {
        assert_eq!(
            searchable_text(&json!({ "@type": "messageChatAddMembers", "member_user_ids": [1] })),
            ""
        );
    }

    #[test]
    fn reads_legacy_deletability_and_chat_sender() {
        let msg = json!({
            "id": 1, "date": 0,
            "sender_id": { "@type": "messageSenderChat", "chat_id": -100 },
            "can_be_deleted_only_for_self": false,
            "can_be_deleted_for_all_users": true,
            "content": { "@type": "messageSticker" }
        });
        let info = MessageInfo::from_json(&msg).unwrap();
        assert_eq!(info.sender, SenderId::Chat(-100));
        assert_eq!(
            info.deletability,
            Some(Deletability {
                for_self: false,
                for_everyone: true
            })
        );
    }
}
