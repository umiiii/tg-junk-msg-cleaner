//! Checks the requests this tool sends against a real libtdjson. Run with
//! `TDJSON_PATH=/path/to/libtdjson.so cargo test -- --ignored`.
//!
//! No Telegram account or network is needed: TDLib parses every request before it
//! checks authorization, so a malformed request fails differently from an
//! unauthorized one.

use std::sync::Arc;
use std::time::Duration;

use crate::auth::{authorize, TdlibParams};
use crate::client::{configure_logging, tdlib_version, Client};
use crate::fake::ScriptedPrompter;
use crate::tdjson::TdJson;
use crate::telegram::{ChatRef, Telegram};

fn assert_parsed<T: std::fmt::Debug, E: std::fmt::Display>(what: &str, result: Result<T, E>) {
    match result {
        Err(err) => {
            let message = err.to_string();
            eprintln!("{what}: {message}");
            let lower = message.to_lowercase();
            for bad in ["parse", "unknown", "timed out"] {
                assert!(!lower.contains(bad), "{what}: {message}");
            }
        }
        Ok(value) => eprintln!("{what}: ok {value:?}"),
    }
}

#[test]
#[ignore = "needs TDJSON_PATH pointing at a real libtdjson"]
fn real_tdlib_accepts_every_request() {
    let path = std::env::var_os("TDJSON_PATH").expect("set TDJSON_PATH");
    let library = TdJson::load(Some(path.as_ref())).unwrap();
    eprintln!("TDLib {:?}", tdlib_version(&library));
    configure_logging(&library, 0, None);
    let data = std::env::temp_dir().join(format!("tg-cleaner-real-tdlib-{}", std::process::id()));
    let client = Client::new(Arc::new(library));
    let params = TdlibParams {
        api_id: 94575,
        api_hash: "a3406de8d171bb422bb6ddf3bbd800e2".into(),
        database_directory: data.clone(),
        use_test_dc: true,
    };
    // No scripted answers: authorization stops once TDLib asks for the phone number,
    // which proves setTdlibParameters was accepted.
    let err = authorize(&client, &params, &mut ScriptedPrompter::default()).unwrap_err();
    assert!(err.to_string().contains("no scripted answer"), "{err:#}");

    let tg = Telegram::new(client);
    let chat = -1001234567890;
    assert_parsed("getMe", tg.me().map(|me| me.id));
    assert_parsed("getChat", tg.chat(chat).map(|c| c.id));
    assert_parsed("getChats/loadChats", tg.chats(5).map(|c| c.len()));
    assert_parsed(
        "searchPublicChat",
        tg.resolve_chat(&ChatRef::Username("telegram".into())).map(|c| c.id),
    );
    assert_parsed(
        "searchChats",
        tg.resolve_chat(&ChatRef::Title("spam".into())).map(|c| c.id),
    );
    assert_parsed("getChatHistory", tg.history(chat, 0));
    assert_parsed("searchChatMessages", tg.search(chat, "spam", 0));
    assert_parsed("getMessageProperties", tg.deletability(chat, 1 << 20));
    assert_parsed("deleteMessages", tg.delete_messages(chat, &[1 << 20], true));
    drop(tg);
    let _ = std::fs::remove_dir_all(data);
    std::thread::sleep(Duration::from_millis(100));
}
