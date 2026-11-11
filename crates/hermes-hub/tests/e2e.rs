//! End-to-end: enroll a node and a device, connect both, run a command, then
//! complete a secret prompt and prove the value never reached the audit log.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use hermes_hub::{Config, HubState};
use hermes_node::{Agent, Config as NodeConfig};
use hermes_protocol::{
    CommandRequest, CommandStatus, DeviceToHub, Frame, HubToDevice, PromptAnswer, PromptDecision,
    PtySpec, SecretSpec, StreamChannel,
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
        pty: None,
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
        pty: None,
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
                pty: None,
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

#[tokio::test]
async fn terminal_input_is_relayed_and_never_stored() {
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
    let (node_id, node_token) = enroll(&client, &base, "node", "zephyr-e2e").await;
    let (device_id, device_token) = enroll(&client, &base, "device", "laptop-terminal").await;
    let root = std::env::temp_dir().join(format!("hermes-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("root");
    let node = Agent::new(NodeConfig {
        hub_url: format!("ws://{address}/v1/node"),
        token: node_token,
        node_id: node_id.clone(),
        name: "zephyr-e2e".into(),
        allowed_roots: vec![root.clone()],
        allowed_commands: vec!["sh".into()],
        max_concurrency: 2,
    });
    let node_capabilities = node.capabilities().clone();
    assert!(node_capabilities.pty, "a unix node advertises a terminal");
    let node_task = tokio::spawn(async move { node.run().await });
    wait_for_node(&client, &base, &node_id).await;
    let mut device = connect_device(&base, &device_id, &device_token).await;

    let typed = "hello-terminal";
    send_device(
        &mut device,
        DeviceToHub::SubmitCommand {
            node_id,
            command: CommandRequest {
                id: "cmd-terminal".into(),
                idempotency_key: "idem-terminal".into(),
                argv: vec![
                    "sh".into(),
                    "-c".into(),
                    "printf ready; read line; printf 'got:%s' \"$line\"; sleep 30".to_string(),
                ],
                cwd: Some(root.display().to_string()),
                env: BTreeMap::new(),
                timeout_ms: 0,
                pty: Some(PtySpec { cols: 80, rows: 24 }),
                secret: None,
            },
        },
    )
    .await;

    let mut terminal = String::new();
    let mut sent_input = false;
    let mut status = None;
    while status.is_none() {
        match next_device(&mut device).await {
            HubToDevice::CommandState { command } if command.id == "cmd-terminal" => {
                if command.status.is_terminal() {
                    status = Some(command.status);
                }
            }
            HubToDevice::CommandStream {
                command_id,
                channel,
                data_b64,
                ..
            } if command_id == "cmd-terminal" => {
                let bytes = STANDARD.decode(data_b64).expect("base64");
                if channel == StreamChannel::Pty {
                    terminal.push_str(&String::from_utf8_lossy(&bytes));
                }
                if !sent_input && terminal.contains("ready") {
                    sent_input = true;
                    send_device(
                        &mut device,
                        DeviceToHub::TerminalInput {
                            command_id: "cmd-terminal".into(),
                            data_b64: STANDARD.encode(format!("{typed}\r")),
                        },
                    )
                    .await;
                }
                if terminal.contains(typed) {
                    send_device(
                        &mut device,
                        DeviceToHub::CancelCommand {
                            command_id: "cmd-terminal".into(),
                        },
                    )
                    .await;
                }
            }
            HubToDevice::Error { code, message } => panic!("hub error {code}: {message}"),
            _ => {}
        }
    }
    assert!(
        terminal.contains(&format!("got:{typed}")),
        "terminal never echoed the typed line: {terminal:?}"
    );
    assert_eq!(status, Some(CommandStatus::Cancelled));

    // The hub relays terminal bytes but keeps none of them.
    let events: Value = client
        .get(format!("{base}/api/v1/commands/cmd-terminal/events"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("events")
        .json()
        .await
        .expect("events json");
    assert_eq!(
        events["events"].as_array().map(Vec::len),
        Some(0),
        "terminal output must not be persisted: {events}"
    );

    node_task.abort();
    server.abort();
}

async fn command_status(client: &reqwest::Client, base: &str, id: &str) -> String {
    let body: Value = client
        .get(format!("{base}/api/v1/commands/{id}"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("command")
        .error_for_status()
        .expect("command status")
        .json()
        .await
        .expect("command json");
    body["status"].as_str().unwrap_or("").to_string()
}

async fn wait_for_status(client: &reqwest::Client, base: &str, id: &str, want: &str) {
    let mut last = String::new();
    for _ in 0..100 {
        last = command_status(client, base, id).await;
        if last == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("command {id} stayed {last:?}, wanted {want}");
}

fn pty_shell(id: &str, key: &str, root: &std::path::Path) -> CommandRequest {
    CommandRequest {
        id: id.into(),
        idempotency_key: key.into(),
        // An interactive shell, matching a terminal the user left open.
        argv: vec!["sh".into()],
        cwd: Some(root.display().to_string()),
        env: BTreeMap::new(),
        timeout_ms: 0,
        pty: Some(PtySpec { cols: 80, rows: 24 }),
        secret: None,
    }
}

struct LiveHub {
    base: String,
    client: reqwest::Client,
    node_id: String,
    root: std::path::PathBuf,
    node_task: tokio::task::JoinHandle<()>,
    server: tokio::task::JoinHandle<()>,
}

async fn start_live_hub(node_name: &str) -> LiveHub {
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
    let (node_id, node_token) = enroll(&client, &base, "node", node_name).await;
    let root = std::env::temp_dir().join(format!("hermes-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("root");
    let node = Agent::new(NodeConfig {
        hub_url: format!("ws://{address}/v1/node"),
        token: node_token,
        node_id: node_id.clone(),
        name: node_name.into(),
        allowed_roots: vec![root.clone()],
        allowed_commands: vec!["sh".into()],
        max_concurrency: 4,
    });
    let node_task = tokio::spawn(async move {
        let _ = node.run().await;
    });
    wait_for_node(&client, &base, &node_id).await;
    LiveHub {
        base,
        client,
        node_id,
        root,
        node_task,
        server,
    }
}

impl LiveHub {
    fn stop(self) {
        self.node_task.abort();
        self.server.abort();
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// Revoking a device must hang up the terminal it opened, and must not take
/// anyone else's command with it. Issue: an abandoned shell outlived revoke.
#[tokio::test]
async fn revoke_cancels_that_devices_terminal_only() {
    init_tracing();
    let hub = start_live_hub("forge-revoke").await;
    let (device_a, token_a) = enroll(&hub.client, &hub.base, "device", "laptop-a").await;
    let (device_b, token_b) = enroll(&hub.client, &hub.base, "device", "laptop-b").await;
    let mut device_a_sock = connect_device(&hub.base, &device_a, &token_a).await;
    let mut device_b_sock = connect_device(&hub.base, &device_b, &token_b).await;

    send_device(
        &mut device_a_sock,
        DeviceToHub::SubmitCommand {
            node_id: hub.node_id.clone(),
            command: pty_shell("cmd-a", "idem-a", &hub.root),
        },
    )
    .await;
    send_device(
        &mut device_b_sock,
        DeviceToHub::SubmitCommand {
            node_id: hub.node_id.clone(),
            command: pty_shell("cmd-b", "idem-b", &hub.root),
        },
    )
    .await;
    let owner = hub
        .client
        .post(format!("{}/api/v1/commands", hub.base))
        .bearer_auth(ADMIN)
        .json(&json!({
            "node_id": hub.node_id,
            "id": "cmd-owner",
            "idempotency_key": "idem-owner",
            "argv": ["sh", "-c", "sleep 60"],
            "cwd": hub.root.display().to_string(),
            "timeout_ms": 60_000
        }))
        .send()
        .await
        .expect("owner command")
        .error_for_status()
        .expect("owner command status");
    assert!(owner.status().is_success());

    wait_for_status(&hub.client, &hub.base, "cmd-a", "running").await;
    wait_for_status(&hub.client, &hub.base, "cmd-b", "running").await;
    wait_for_status(&hub.client, &hub.base, "cmd-owner", "running").await;

    let revoke = hub
        .client
        .delete(format!("{}/api/v1/devices/{device_a}", hub.base))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("revoke");
    assert!(revoke.status().is_success(), "{}", revoke.status());

    wait_for_status(&hub.client, &hub.base, "cmd-a", "cancelled").await;
    assert_eq!(
        command_status(&hub.client, &hub.base, "cmd-b").await,
        "running",
        "another device's terminal must keep running"
    );
    assert_eq!(
        command_status(&hub.client, &hub.base, "cmd-owner").await,
        "running",
        "an owner API command has no device and must keep running"
    );

    // The revoked token cannot open a new socket.
    let url = format!("{}/v1/device", hub.base.replacen("http", "ws", 1));
    let mut request = url.as_str().into_client_request().expect("ws request");
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token_a}")).expect("header"),
    );
    let refused = tokio_tungstenite::connect_async(request).await;
    assert!(
        refused.is_err(),
        "a revoked device token must not complete the WebSocket handshake"
    );

    hub.stop();
}

/// Closing the current device socket hangs the terminal up. A socket that was
/// only replaced by a reconnect must not.
#[tokio::test]
async fn disconnect_cancels_the_terminal_but_replacement_does_not() {
    init_tracing();
    let hub = start_live_hub("forge-disconnect").await;
    let (device_id, token) = enroll(&hub.client, &hub.base, "device", "laptop-drop").await;
    let mut first = connect_device(&hub.base, &device_id, &token).await;
    send_device(
        &mut first,
        DeviceToHub::SubmitCommand {
            node_id: hub.node_id.clone(),
            command: pty_shell("cmd-live", "idem-live", &hub.root),
        },
    )
    .await;
    wait_for_status(&hub.client, &hub.base, "cmd-live", "running").await;

    // The second hello replaces the first socket. The shell stays. Wait past
    // the node's hangup grace so a mistaken cancel would already show.
    let second = connect_device(&hub.base, &device_id, &token).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        command_status(&hub.client, &hub.base, "cmd-live").await,
        "running",
        "replacing the device socket must not kill its terminal"
    );

    let mut second = second;
    second.close(None).await.expect("close device socket");
    drop(second);
    drop(first);
    wait_for_status(&hub.client, &hub.base, "cmd-live", "cancelled").await;

    hub.stop();
}

fn hub_config() -> Config {
    Config {
        admin_token: ADMIN.into(),
        ..Config::for_test()
    }
}
