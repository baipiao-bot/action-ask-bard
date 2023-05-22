use bard_api_rs::ChatSession;
use ezio::prelude::*;
use isahc::AsyncReadResponseExt;
use libaes::Cipher;
use redis::AsyncCommands;
use std::{env, time::Duration};
use teloxide::types::Message;
use teloxide::types::ParseMode::MarkdownV2;
use teloxide::{
    payloads::SendMessage,
    types::{Update, UpdateKind},
};
use tokio::{sync::broadcast, time::interval};
fn decrypt(data: &[u8], secret: &[u8]) -> String {
    let key = &secret[0..32];
    let iv = &secret[32..(32 + 16)];
    let cipher = Cipher::new_256(key.try_into().unwrap());
    String::from_utf8(cipher.cbc_decrypt(iv, data)).unwrap()
}

fn escape(text: &str) -> String {
    let mut result = text
        .replace('\"', "\\\"")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('(', "\\(")
        .replace(')', "\\)")
        .replace('>', "\\>")
        .replace('<', "\\<")
        .replace('_', "\\_")
        .replace('-', "\\-")
        .replace('.', "\\.")
        .replace('!', "\\!");
    let re = regex::Regex::new(r"\[([^]]+)\]\\\(([^)]+)\\\)");
    if let Ok(re) = re {
        result = re.replace_all(&result, "[$1]($2)").to_string();
    }
    let re = regex::Regex::new(r"^(\s*)\*\s");
    if let Ok(re) = re {
        result = re.replace_all(&result, "$1- ").to_string();
    }
    result
}

#[tokio::main]
async fn main() {
    let secret_str = env::var("SECRET").unwrap();
    let redis_url = env::var("REDIS_URL").unwrap();
    let telegram_token = env::var("TELEGRAM_TOKEN").unwrap();

    let secret = hex::decode(secret_str).unwrap();
    let request_encrypted = file::read("./request.json.encrypted");
    let request_str = decrypt(&hex::decode(request_encrypted).unwrap(), &secret);
    let update: Update = serde_json::from_str(&request_str).unwrap();
    let redis_client = redis::Client::open(redis_url).unwrap();
    let mut redis_connection = redis_client.get_async_connection().await.unwrap();
    let UpdateKind::Message(message) = update.kind else { unimplemented!("") };
    let mut chat_session =
        if let Some(reply_to_message_id) = message.reply_to_message().map(|it| it.id) {
            let key = format!("{}-{}", message.chat.id, reply_to_message_id);
            let corresponding_session: Result<String, redis::RedisError> =
                redis_connection.get(key).await;
            if let Ok(corresponding_session) = corresponding_session {
                serde_json::from_str(&corresponding_session).unwrap()
            } else {
                ChatSession::new().await
            }
        } else {
            ChatSession::new().await
        };

    let (stop_typing_action_tx, mut stop_typing_action_rx) = broadcast::channel(1);
    let chat_id = message.chat.id;
    let telegram_token_cloned = telegram_token.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = stop_typing_action_rx.recv() => {
                    break;
                }
                _ = interval.tick() => {
                    let send_message_request: isahc::Request<String> = isahc::Request::builder()
                        .uri(format!("https://api.telegram.org/bot{telegram_token_cloned}/sendChatAction"))
                        .header("Content-Type", "application/json")
                        .method("POST")
                        .body(format!("{{\"chat_id\": {chat_id}, \"action\": \"typing\"}}"))
                        .unwrap();
                    isahc::send_async(send_message_request).await.unwrap();
                }
            }
        }
    });

    let response = chat_session.send_message(message.text().unwrap()).await;
    let escaped_message = escape(&response);
    let mut telegram_response = SendMessage::new(message.chat.id, escaped_message.clone());
    telegram_response.reply_to_message_id = Some(message.id);
    telegram_response.parse_mode = Some(MarkdownV2);
    let response_json = serde_json::to_string(&telegram_response).unwrap();
    let url = format!("https://api.telegram.org/bot{telegram_token}/sendMessage");
    let session_str = serde_json::to_string(&chat_session).unwrap();
    let send_message_request = isahc::Request::builder()
        .uri(url)
        .header("Content-Type", "application/json")
        .method("POST")
        .body(response_json.clone())
        .unwrap();
    let mut send_message_response = isahc::send_async(send_message_request)
        .await
        .map_err(|_err| panic!("failed to send message: {response_json}"))
        .unwrap();
    stop_typing_action_tx.send(()).unwrap();
    let mut send_message_response: serde_json::Value = send_message_response
        .json()
        .await
        .map_err(|_err| panic!("failed to send message: {response_json}"))
        .unwrap();
    println!("{:?}\n{}", send_message_response, escaped_message);
    let send_message: Message =
        serde_json::from_value(send_message_response["result"].take()).unwrap();
    let key = format!("{}-{}", send_message.chat.id, send_message.id);
    let _: () = redis_connection
        .set_ex(key, session_str, 60 * 60)
        .await
        .unwrap();
}
