use fake::{
    Fake,
    faker::{
        company::en::{Buzzword, CatchPhrase},
        internet::en::SafeEmail,
        lorem::en::Paragraph,
        name::en::Name,
    },
};
use lettre::{
    Address, AsyncSmtpTransport, AsyncTransport, Message, SmtpTransport, Tokio1Executor, Transport,
    address::Envelope,
    message::{Attachment, MultiPart, SinglePart, header::ContentType},
    transport::smtp::response::Response,
};
use mailcrab::MailMessageMetadata;
use reqwest::Client;
use std::ffi::OsStr;
use tokio::time::{Duration, sleep};

use crate::{parse_env_var, run};

async fn send_message(
    with_html: bool,
    with_plain: bool,
    with_attachment: bool,
) -> Result<Response, Box<dyn std::error::Error>> {
    let smtp_port: u16 = parse_env_var("SMTP_PORT", 1025);
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1".to_string())
        .port(smtp_port)
        .build();

    let to: String = SafeEmail().fake();
    let to_name: String = Name().fake();
    let from: String = SafeEmail().fake();
    let from_name: String = Name().fake();
    let body: String = [
        Paragraph(2..3).fake::<String>(),
        Paragraph(2..3).fake::<String>(),
        Paragraph(2..3).fake::<String>(),
    ]
    .join("\n");
    let html: String = format!(
        "{}\n<p><a href=\"https://github.com/tweedegolf/mailcrab\">external link</a></p>",
        body.replace('\n', "<br>\n")
    );

    let builder = Message::builder()
        .from(format!("{from_name} <{from}>",).parse()?)
        .to(format!("{to_name} <{to}>").parse()?)
        .subject(CatchPhrase().fake::<String>());

    let mut multipart = MultiPart::mixed().build();

    match (with_html, with_plain) {
        (true, true) => {
            multipart = multipart.multipart(
                MultiPart::alternative()
                    .singlepart(SinglePart::plain(body))
                    .singlepart(SinglePart::html(html)),
            );
        }
        (false, true) => {
            multipart = multipart.singlepart(SinglePart::plain(body));
        }
        (true, false) => {
            multipart = multipart.singlepart(SinglePart::html(html));
        }
        _ => panic!("Email should have html or plain body"),
    };

    if with_attachment {
        let filebody = std::fs::read("blank.pdf")?;
        let content_type = ContentType::parse("application/pdf")?;
        let filename = format!("{}.pdf", Buzzword().fake::<&str>().to_ascii_lowercase());
        let attachment = Attachment::new(filename).body(filebody.clone(), content_type.clone());
        multipart = multipart.singlepart(attachment);
    }

    let email = builder.multipart(multipart)?;

    let response = mailer.send(email).await?;

    Ok(response)
}

async fn get_messages_metadata() -> Result<Vec<MailMessageMetadata>, Box<dyn std::error::Error>> {
    let http_port: u16 = parse_env_var("HTTP_PORT", 1080);

    let client = Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let mails: Vec<MailMessageMetadata> = client
        .get(format!("http://127.0.0.1:{http_port}/api/messages"))
        .send()
        .await?
        .json()
        .await?;

    Ok(mails)
}

async fn test_receive_messages() -> Result<Vec<Response>, Box<dyn std::error::Error>> {
    let mut responses = vec![];

    responses.push(send_message(true, true, false).await?);
    responses.push(send_message(true, false, false).await?);
    responses.push(send_message(false, true, true).await?);

    Ok(responses)
}

#[tokio::test]
async fn functional() {
    let join = tokio::task::spawn(run());

    // wait for mailcrab to startup
    for _i in 0..60 {
        if get_messages_metadata().await.is_ok() {
            break;
        }

        sleep(Duration::from_millis(100)).await;
    }

    // send messages and retrieve the message id from mailcrab
    let responses = test_receive_messages()
        .await
        .unwrap()
        .into_iter()
        .map(|r| {
            r.message()
                .next()
                .unwrap_or_default()
                .split_ascii_whitespace()
                .last()
                .unwrap_or_default()
                .to_owned()
        })
        .collect::<Vec<String>>();

    // fetch message metadata from mailcrab
    let messages = get_messages_metadata().await.unwrap();

    // sorted fetched message metadata from mailcrab and sort them by sent message ids
    let mut sorted_messages = vec![];
    for id in &responses {
        if let Some(message) = messages.iter().find(|m| m.id.to_string() == *id) {
            sorted_messages.push(message.clone());
        }
    }

    assert_eq!(sorted_messages.len(), 3);
    assert!(sorted_messages[0].has_html);
    assert!(sorted_messages[0].has_plain);
    assert!(sorted_messages[0].attachments.is_empty());

    assert!(sorted_messages[1].has_html);
    assert!(!sorted_messages[1].has_plain);
    assert!(sorted_messages[1].attachments.is_empty());

    assert!(!sorted_messages[2].has_html);
    assert!(sorted_messages[2].has_plain);
    assert_eq!(sorted_messages[2].attachments.len(), 1);

    // send a large attachment and verify it can be downloaded via the URL endpoint
    const SIZE: usize = 75 * 1024 * 1024; // 75 MiB
    send_large_file(SIZE).await.expect("send failed");

    let mut large_meta = None;
    for _ in 0..300 {
        let messages = get_messages_metadata().await.unwrap();
        if let Some(m) = messages
            .into_iter()
            .find(|m| m.attachments.iter().any(|a| a.filename == "large.bin"))
        {
            large_meta = Some(m);
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    let meta = large_meta.expect("large attachment message not received within timeout");

    assert_eq!(meta.attachments.len(), 1);
    assert_eq!(meta.attachments[0].filename, "large.bin");

    let http_port: u16 = parse_env_var("HTTP_PORT", 1080);
    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let attachment_bytes = client
        .get(format!(
            "http://127.0.0.1:{http_port}/api/message/{}/attachment/0",
            meta.id
        ))
        .send()
        .await
        .expect("attachment request failed")
        .bytes()
        .await
        .expect("reading body failed");

    assert_eq!(attachment_bytes.len(), SIZE);

    let expected: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
    assert_eq!(attachment_bytes.as_ref(), expected.as_slice());

    test_sms().await;

    // stop the server
    join.abort();
}

/// send an SMS through the Sinch mock and receive its delivery reports
async fn test_sms() {
    let http_port: u16 = parse_env_var("HTTP_PORT", 1080);
    let sinch_port: u16 = parse_env_var("SINCH_PORT", 1090);
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // a webhook target that forwards received callbacks to the test
    let (callback_tx, mut callback_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(8);
    let callback_app = axum::Router::new().route(
        "/status",
        axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let callback_tx = callback_tx.clone();
            async move {
                callback_tx.send(body).await.unwrap();
                axum::http::StatusCode::NO_CONTENT
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_url = format!("http://{}/status", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, callback_app).await });

    let token: serde_json::Value = client
        .post(format!("http://127.0.0.1:{sinch_port}/oauth2/token"))
        .basic_auth("key", Some("secret"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("grant_type=client_credentials")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = token["access_token"].as_str().unwrap();

    let sent: serde_json::Value = client
        .post(format!(
            "http://127.0.0.1:{sinch_port}/v1/projects/project/messages:send"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "app_id": "app",
            "recipient": { "identified_by": { "channel_identities": [
                { "channel": "SMS", "identity": "+4791234567" }
            ]}},
            "message": { "text_message": { "text": "Hello from the test" } },
            "channel_properties": { "SMS_SENDER": "Pincer" },
            "message_metadata": "{\"orderId\":\"order\"}",
            "callback_url": callback_url,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let message_id = sent["message_id"].as_str().unwrap();
    assert_eq!(message_id.len(), 26);

    let messages: Vec<serde_json::Value> = client
        .get(format!("http://127.0.0.1:{http_port}/api/sms"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(messages.iter().any(|m| m["message_id"] == message_id));

    for expected in ["QUEUED_ON_CHANNEL", "DELIVERED"] {
        let callback = tokio::time::timeout(Duration::from_secs(5), callback_rx.recv())
            .await
            .expect("delivery report not received within timeout")
            .unwrap();
        let report = &callback["message_delivery_report"];
        assert_eq!(report["status"], expected);
        assert_eq!(report["message_id"], message_id);
        assert_eq!(report["metadata"], "{\"orderId\":\"order\"}");
    }

    // send an SMS and return its internal id
    let send = |recipient: serde_json::Value| {
        let client = client.clone();
        let callback_url = callback_url.clone();
        async move {
            let sent: serde_json::Value = client
                .post(format!(
                    "http://127.0.0.1:{sinch_port}/v1/projects/project/messages:send"
                ))
                .bearer_auth("token")
                .json(&serde_json::json!({
                    "app_id": "app",
                    "recipient": recipient,
                    "message": { "text_message": { "text": "Rules" } },
                    "callback_url": callback_url,
                }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let messages: Vec<serde_json::Value> = client
                .get(format!("http://127.0.0.1:{http_port}/api/sms"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            messages
                .iter()
                .find(|m| m["message_id"] == sent["message_id"])
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let sms_number = |number: &str| {
        serde_json::json!({ "identified_by": { "channel_identities": [
            { "channel": "SMS", "identity": number }
        ]}})
    };
    let finalize = |id: String, status: &'static str| {
        let client = client.clone();
        async move {
            client
                .post(format!(
                    "http://127.0.0.1:{http_port}/api/sms/{id}/status/{status}"
                ))
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    let mut next_status = async || {
        let callback = tokio::time::timeout(Duration::from_secs(5), callback_rx.recv())
            .await
            .expect("delivery report not received within timeout")
            .unwrap();
        callback["message_delivery_report"]["status"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // numbers ending in 0001 fail
    send(sms_number("+4791230001")).await;
    assert_eq!(next_status().await, "QUEUED_ON_CHANNEL");
    assert_eq!(next_status().await, "FAILED");

    // numbers ending in 0002 stay pending until finalized from the UI, once
    let id = send(sms_number("+4791230002")).await;
    assert_eq!(next_status().await, "QUEUED_ON_CHANNEL");
    assert_eq!(finalize(id.clone(), "DELIVERED").await, 200);
    assert_eq!(next_status().await, "DELIVERED");
    assert_eq!(finalize(id.clone(), "FAILED").await, 409);
    assert_eq!(finalize(id, "QUEUED_ON_CHANNEL").await, 400);

    // settings changed from the web interface apply to new messages
    let mut info: serde_json::Value = client
        .get(format!("http://127.0.0.1:{http_port}/api/sinch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    info["settings"]["dlr_delay_ms"] = 100.into();
    info["settings"]["dlr_rules"] = serde_json::json!([{ "suffix": "4567", "outcome": "failed" }]);
    let status = client
        .put(format!("http://127.0.0.1:{http_port}/api/sinch/settings"))
        .json(&info["settings"])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 200);
    send(sms_number("+4791234567")).await;
    assert_eq!(next_status().await, "QUEUED_ON_CHANNEL");
    assert_eq!(next_status().await, "FAILED");

    info["settings"]["webhook_url"] = "not a url".into();
    let status = client
        .put(format!("http://127.0.0.1:{http_port}/api/sinch/settings"))
        .json(&info["settings"])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 400);

    // finalizing before the automatic reports sends QUEUED_ON_CHANNEL first and
    // stops the automatic reports
    let id = send(serde_json::json!({ "contact_id": "contact" })).await;
    assert_eq!(finalize(id, "FAILED").await, 200);
    assert_eq!(next_status().await, "QUEUED_ON_CHANNEL");
    assert_eq!(next_status().await, "FAILED");
    let more = tokio::time::timeout(Duration::from_millis(2500), callback_rx.recv()).await;
    assert!(more.is_err(), "no reports after the final report");

    // SMS can be deleted through the REST API like email
    let messages: Vec<serde_json::Value> = client
        .get(format!("http://127.0.0.1:{http_port}/api/sms"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = messages[0]["id"].as_str().unwrap();
    for expected in [200, 404] {
        let status = client
            .post(format!("http://127.0.0.1:{http_port}/api/delete/{id}"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, expected);
    }

    // requests without credentials are rejected
    let status = client
        .post(format!(
            "http://127.0.0.1:{sinch_port}/v1/projects/project/messages:send"
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 401);
}

#[tokio::test]
#[ignore]
async fn send_sample_messages() {
    let smtp_port: u16 = parse_env_var("SMTP_PORT", 1025);
    let mut paths = std::fs::read_dir("../samples").unwrap();
    let mailer = SmtpTransport::builder_dangerous("127.0.0.1".to_string())
        .port(smtp_port)
        .build();

    while let Some(Ok(entry)) = paths.next() {
        // skip non *.email files
        if entry.path().extension() != Some(OsStr::new("email")) {
            continue;
        }

        let message = std::fs::read_to_string(entry.path()).unwrap();
        let mut lines = message.lines();

        let sender = lines
            .next()
            .unwrap()
            .trim_start_matches("Sender: ")
            .parse::<Address>()
            .unwrap();
        let recipients = lines
            .next()
            .unwrap()
            .trim_start_matches("Recipients: ")
            .split(',')
            .map(|r| r.trim().parse::<Address>().unwrap())
            .collect::<Vec<Address>>();
        let envelope = Envelope::new(Some(sender), recipients).unwrap();

        let email = lines.collect::<Vec<&str>>().join("\n");

        mailer.send_raw(&envelope, email.as_bytes()).unwrap();
    }
}

async fn send_large_file(size_bytes: usize) -> Result<Response, Box<dyn std::error::Error>> {
    let smtp_port: u16 = parse_env_var("SMTP_PORT", 1025);
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1".to_string())
        .port(smtp_port)
        .build();

    // generates pseudo-random bytes without any added dependencies
    let body: Vec<u8> = (0..size_bytes).map(|i| (i % 251) as u8).collect();

    let email = Message::builder()
        .from("sender@example.com".parse()?)
        .to("recipient@example.com".parse()?)
        .subject(format!("Large attachment test ({size_bytes} bytes)"))
        .multipart(
            MultiPart::mixed()
                .singlepart(SinglePart::plain("See attached.".to_owned()))
                .singlepart(
                    Attachment::new("large.bin".to_owned())
                        .body(body, ContentType::parse("application/octet-stream")?),
                ),
        )?;

    Ok(mailer.send(email).await?)
}
