//! End-to-end: enroll a node and a device, connect both, run a command, then
//! complete a secret prompt and prove the value never reached the audit log.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use hermes_hub::{Config, HubState};
use hermes_node::{Agent, Config as NodeConfig};
use hermes_protocol::{
    CommandRequest, CommandStatus, DeviceToHub, Frame, HubToDevice, PromptAnswer, PromptDecision,
    SecretSpec,
};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const ADMIN: &str = "e2e-admin-token-that-is-long-enough";

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("hermes_node=debug,hermes_hub=debug")
            }),
        )
        .with_test_writer()
        .try_init();
}

type DeviceSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn enroll(client: &reqwest::Client, base: &str, kind: &str, name: &str) -> (String, String) {
    let response: Value = client
        .post(format!("{base}/api/v1/enrollments"))
        .bearer_auth(ADMIN)
        .json(&json!({ "kind": kind, "name": name }))
        .send()
        .await
        .expect("mint enrollment")
        .error_for_status()
        .expect("mint status")
        .json()
        .await
        .expect("mint json");
    let code = response["code"].as_str().expect("code").to_string();
    let response: Value = client
        .post(format!("{base}/api/v1/enroll"))
        .json(&json!({ "code": code }))
        .send()
        .await
        .expect("enroll")
        .error_for_status()
        .expect("enroll status")
        .json()
        .await
        .expect("enroll json");
    (
        response["id"].as_str().expect("id").to_string(),
        response["token"].as_str().expect("token").to_string(),
    )
}

async fn connect_device(base: &str, device_id: &str, token: &str) -> DeviceSocket {
    let url = format!("{}/v1/device", base.replacen("http", "ws", 1));
    let mut request = url.as_str().into_client_request().expect("ws request");
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).expect("header"),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("device connect");
    send_device(
        &mut socket,
        DeviceToHub::Hello {
            device_id: device_id.into(),
            name: "e2e".into(),
        },
    )
    .await;
    let ack = next_device(&mut socket).await;
    assert!(matches!(ack, HubToDevice::HelloAck { .. }), "{ack:?}");
    let nodes = next_device(&mut socket).await;
    assert!(matches!(nodes, HubToDevice::NodeList { .. }), "{nodes:?}");
    socket
}

async fn send_device(socket: &mut DeviceSocket, message: DeviceToHub) {
    socket
        .send(Message::Text(
            Frame::new(message).encode().expect("encode").into(),
        ))
        .await
        .expect("send");
}

async fn next_device(socket: &mut DeviceSocket) -> HubToDevice {
    loop {
        let message = timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("device frame timeout")
            .expect("device stream ended")
            .expect("device frame");
        if let Message::Text(text) = message {
            return Frame::<HubToDevice>::decode(text.as_str())
                .expect("device frame decode")
                .message;
        }
    }
}

async fn wait_for_node(client: &reqwest::Client, base: &str, node_id: &str) {
    for _ in 0..100 {
        let nodes: Value = client
            .get(format!("{base}/api/v1/nodes"))
            .bearer_auth(ADMIN)
            .send()
            .await
            .expect("nodes")
            .json()
            .await
            .expect("nodes json");
        if nodes["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|node| node["id"] == node_id && node["online"] == true)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("node {node_id} never came online");
}

#[tokio::test]
async fn command_and_secret_prompt_flow_end_to_end() {
    init_tracing();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    let base = format!("http://{address}");
    let state = HubState::in_memory(hub_config());
    let server = tokio::spawn(async move {
        axum::serve(listener, state.router()).await.expect("serve");
    });

    let client = reqwest::Client::new();
    let (node_id, node_token) = enroll(&client, &base, "node", "forge-e2e").await;
    let (device_id, device_token) = enroll(&client, &base, "device", "laptop-e2e").await;

    let root = std::env::temp_dir().join(format!("hermes-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("root");
    let node = Agent::new(NodeConfig {
        hub_url: format!("ws://{address}/v1/node"),
        token: node_token,
        node_id: node_id.clone(),
        name: "forge-e2e".into(),
        allowed_roots: vec![root.clone()],
        allowed_commands: vec!["sh".into(), "sleep".into()],
        max_concurrency: 2,
    });
    let node_task = tokio::spawn(async move { node.run().await });
    wait_for_node(&client, &base, &node_id).await;

    let mut device = connect_device(&base, &device_id, &device_token).await;

    let command = CommandRequest {
        id: "cmd-e2e".into(),
        idempotency_key: "idem-e2e".into(),
        argv: vec!["sh".into(), "-c".into(), "printf hello".into()],
        cwd: Some(root.display().to_string()),
        env: BTreeMap::new(),
        timeout_ms: 5_000,
        secret: None,
    };
    send_device(
        &mut device,
        DeviceToHub::SubmitCommand {
            node_id: node_id.clone(),
            command: command.clone(),
        },
    )
    .await;

    let mut stdout = String::new();
    let mut status = None;
    while status.is_none() {
        match next_device(&mut device).await {
            HubToDevice::CommandState { command } if command.id == "cmd-e2e" => {
                if command.status.is_terminal() {
                    status = Some(command.status);
                }
            }
            HubToDevice::CommandStream {
                command_id,
                channel,
                data_b64,
                ..
            } if command_id == "cmd-e2e" => {
                if channel == hermes_protocol::StreamChannel::Stdout {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data_b64)
                        .expect("base64");
                    stdout.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            HubToDevice::Error { code, message } => panic!("hub error {code}: {message}"),
            _ => {}
        }
    }
    assert_eq!(stdout, "hello");
    assert_eq!(status, Some(CommandStatus::Succeeded));

    // A command that requires a secret: the node must raise a card and inject
    // the answer into the child environment, never argv.
    let secret = "e2e-super-secret-value";
    let command = CommandRequest {
        id: "cmd-secret".into(),
        idempotency_key: "idem-secret".into(),
        argv: vec![
            "sh".into(),
            "-c".into(),
            "test -n \"$MY_SECRET\" && printf ok".into(),
        ],
        cwd: Some(root.display().to_string()),
        env: BTreeMap::new(),
        timeout_ms: 5_000,
        secret: Some(SecretSpec {
            env_var: "MY_SECRET".into(),
            purpose: "e2e credential".into(),
            scope: "one command".into(),
            timeout_ms: 5_000,
        }),
    };
    send_device(
        &mut device,
        DeviceToHub::SubmitCommand {
            node_id: node_id.clone(),
            command,
        },
    )
    .await;

    let mut prompt = None;
    while prompt.is_none() {
        match next_device(&mut device).await {
            HubToDevice::PromptRequest { prompt: requested } => prompt = Some(requested),
            HubToDevice::Error { code, message } => panic!("hub error {code}: {message}"),
            _ => {}
        }
    }
    let prompt = prompt.expect("prompt");
    assert_eq!(prompt.env_var, "MY_SECRET");
    assert_eq!(prompt.purpose, "e2e credential");
    send_device(
        &mut device,
        DeviceToHub::PromptAnswer {
            answer: PromptAnswer {
                prompt_id: prompt.id.clone(),
                challenge: prompt.challenge.clone(),
                decision: PromptDecision::Provide {
                    value: secret.into(),
                    remember: false,
                },
            },
        },
    )
    .await;

    let mut stdout = String::new();
    let mut status = None;
    while status.is_none() {
        match next_device(&mut device).await {
            HubToDevice::CommandState { command } if command.id == "cmd-secret" => {
                if command.status.is_terminal() {
                    status = Some(command.status);
                }
            }
            HubToDevice::CommandStream {
                command_id,
                channel,
                data_b64,
                ..
            } if command_id == "cmd-secret" => {
                if channel == hermes_protocol::StreamChannel::Stdout {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data_b64)
                        .expect("base64");
                    stdout.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            HubToDevice::PromptClosed { .. } => {}
            HubToDevice::Error { code, message } => panic!("hub error {code}: {message}"),
            _ => {}
        }
    }
    assert_eq!(stdout, "ok");
    assert_eq!(status, Some(CommandStatus::Succeeded));

    let audit: Value = client
        .get(format!("{base}/api/v1/audit?limit=200"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("audit")
        .json()
        .await
        .expect("audit json");
    let audit_text = serde_json::to_string(&audit).expect("audit text");
    assert!(
        !audit_text.contains(secret),
        "the secret value leaked into audit: {audit_text}"
    );
    assert!(audit_text.contains("prompt.answered"), "{audit_text}");

    let prompts: Value = client
        .get(format!("{base}/api/v1/prompts"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("prompts")
        .json()
        .await
        .expect("prompts json");
    assert_eq!(prompts["prompts"].as_array().map(Vec::len), Some(0));

    // Revoking the device invalidates its token immediately.
    let revoke = client
        .delete(format!("{base}/api/v1/devices/{device_id}"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("revoke");
    assert!(revoke.status().is_success(), "{}", revoke.status());
    let ws_url = format!("{}/v1/device", base.replacen("http", "ws", 1));
    let mut request = ws_url.as_str().into_client_request().expect("ws request");
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {device_token}")).expect("header"),
    );
    assert!(
        tokio_tungstenite::connect_async(request).await.is_err(),
        "a revoked device token must not complete the WebSocket handshake"
    );

    node_task.abort();
    server.abort();
}

#[tokio::test]
async fn a_wrong_prompt_challenge_is_rejected_and_burned() {
    init_tracing();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    let base = format!("http://{address}");
    let state = HubState::in_memory(hub_config());
    let server = tokio::spawn(async move {
        axum::serve(listener, state.router()).await.expect("serve");
    });
    let client = reqwest::Client::new();
    let (node_id, node_token) = enroll(&client, &base, "node", "sentry-e2e").await;
    let (device_id, device_token) = enroll(&client, &base, "device", "tablet-e2e").await;
    let root = std::env::temp_dir().join(format!("hermes-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("root");
    let node = Agent::new(NodeConfig {
        hub_url: format!("ws://{address}/v1/node"),
        token: node_token,
        node_id: node_id.clone(),
        name: "sentry-e2e".into(),
        allowed_roots: vec![root.clone()],
        allowed_commands: vec!["sh".into()],
        max_concurrency: 2,
    });
    let node_task = tokio::spawn(async move { node.run().await });
    wait_for_node(&client, &base, &node_id).await;
    let mut device = connect_device(&base, &device_id, &device_token).await;

    send_device(
        &mut device,
        DeviceToHub::SubmitCommand {
            node_id,
            command: CommandRequest {
                id: "cmd-bad-challenge".into(),
                idempotency_key: "idem-bad-challenge".into(),
                argv: vec!["sh".into(), "-c".into(), "true".into()],
                cwd: Some(root.display().to_string()),
                env: BTreeMap::new(),
                timeout_ms: 5_000,
                secret: Some(SecretSpec {
                    env_var: "MY_SECRET".into(),
                    purpose: "e2e".into(),
                    scope: "one command".into(),
                    timeout_ms: 5_000,
                }),
            },
        },
    )
    .await;

    let prompt = loop {
        if let HubToDevice::PromptRequest { prompt } = next_device(&mut device).await {
            break prompt;
        }
    };
    send_device(
        &mut device,
        DeviceToHub::PromptAnswer {
            answer: PromptAnswer {
                prompt_id: prompt.id.clone(),
                challenge: "wrong-challenge".into(),
                decision: PromptDecision::Provide {
                    value: "must-not-pass".into(),
                    remember: false,
                },
            },
        },
    )
    .await;

    let mut rejected = false;
    for _ in 0..6 {
        match next_device(&mut device).await {
            HubToDevice::Error { code, .. } if code == "rejected" => {
                rejected = true;
                break;
            }
            HubToDevice::PromptClosed { .. } => {}
            _ => {}
        }
    }
    assert!(rejected, "wrong challenge must be rejected");
    let prompts: Value = client
        .get(format!("{base}/api/v1/prompts"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("prompts")
        .json()
        .await
        .expect("prompts json");
    assert_eq!(prompts["prompts"].as_array().map(Vec::len), Some(0));

    node_task.abort();
    server.abort();
}

fn hub_config() -> Config {
    Config {
        admin_token: ADMIN.into(),
        ..Config::for_test()
    }
}
