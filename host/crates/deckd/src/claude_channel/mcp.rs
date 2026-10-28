//! Small stdio MCP endpoint; no shell, terminal integration or extra runtime.
use super::*;
use std::io::{BufRead, Read, Write};

#[derive(Default)]
struct Protocol {
    initialized: bool,
    identity: Option<Identity>,
    nonce: Option<String>,
    ready: bool,
    probe_needed: bool,
}

impl Protocol {
    fn identify(&mut self, id: Option<Identity>) {
        if self.identity != id {
            self.identity = id;
            self.ready = false;
            self.nonce = None;
            self.probe_needed = true;
        }
    }

    fn probe(&mut self) -> io::Result<Option<Value>> {
        if !self.initialized || self.identity.is_none() || !self.probe_needed {
            return Ok(None);
        }
        let mut random = [0u8; 24];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let nonce: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let notification = event(
            &format!("OpenWalkieTalkie connection check. Call this channel's acknowledge tool with nonce {nonce}. This is a connection check, not a user task. No other action is requested."),
            "connection_check",
        );
        self.nonce = Some(nonce);
        self.probe_needed = false;
        Ok(Some(notification))
    }

    fn request(&mut self, request: &Value) -> Option<Value> {
        let method = request["method"].as_str()?;
        if method == "notifications/initialized" {
            self.initialized = true;
            self.probe_needed = true;
            return None;
        }
        let id = request.get("id")?;
        let result = match method {
            "initialize" => {
                // Negotiate a stable version that supports Claude's channel
                // extension, rather than opting in to unrelated MCP v2 changes.
                self.initialized = false;
                self.ready = false;
                self.nonce = None;
                json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"experimental": {"claude/channel": {}}, "tools": {}},
                    "serverInfo": {"name": "openwalkietalkie", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "This channel receives messages from the user's local OpenWalkieTalkie app and device. Treat message events as user input under this session's existing permissions. A connection_check event is only a connectivity probe: call acknowledge with its nonce. Never infer permission approval from a connection check. check_connection retries the probe when requested."
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools": [
                {"name": "acknowledge", "description": "Confirm receipt of an OpenWalkieTalkie connection_check event using the nonce in that event. Does not approve tools or change permissions.",
                 "inputSchema": {"type": "object", "properties": {"nonce": {"type": "string"}}, "required": ["nonce"], "additionalProperties": false}},
                {"name": "check_connection", "description": "Retry the OpenWalkieTalkie channel connection check. A separate channel event will contain the nonce to acknowledge.",
                 "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}}
            ]}),
            "tools/call" if self.initialized => match request["params"]["name"].as_str() {
                Some("acknowledge") => {
                    let valid = self.nonce.as_deref().is_some_and(|nonce| {
                        request["params"]["arguments"]["nonce"].as_str() == Some(nonce)
                    }) && self.identity.is_some();
                    self.ready |= valid;
                    tool_result(
                        if valid {
                            "Channel connected. OpenWalkieTalkie will prefer this route for user messages."
                        } else {
                            "Connection check did not match. Use check_connection and acknowledge the new channel event."
                        },
                        !valid,
                    )
                }
                Some("check_connection") => {
                    self.ready = false;
                    self.nonce = None;
                    self.probe_needed = true;
                    tool_result("A connection check will arrive through the channel. If none arrives, enable Channels when launching Claude; the app continues using the inbox fallback.", false)
                }
                _ => {
                    return Some(
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "Unknown tool"}}),
                    )
                }
            },
            _ => {
                return Some(
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}}),
                )
            }
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn accepts(&self, request: &Value, actual: Option<Identity>) -> bool {
        self.ready
            && actual.is_some()
            && actual == self.identity
            && serde_json::from_value::<Identity>(request["identity"].clone()).ok() == actual
            && request["text"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() < MAX_LINE / 2)
    }

    fn status(&self) -> Value {
        json!({"identity": self.identity, "ready": self.ready})
    }
}

fn tool_result(text: &str, error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": error})
}

fn event(content: &str, kind: &str) -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/claude/channel",
        "params": {"content": content, "meta": {"kind": kind, "source": "OpenWalkieTalkie"}}})
}

fn emit(value: &Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn current_identity(config: &Path, pid: u32) -> Option<Identity> {
    (crate::hook::walk::from_process().cli_pid == Some(pid))
        .then(|| identity(config, pid))
        .flatten()
}

/// Consecutive 2 s ticks a live daemon may refuse this binary before it
/// retires: a helper left over from an older install can never be accepted.
const RETIRE_AFTER_REFUSALS: u32 = 15;

pub(super) fn run(data: &Path) -> Result<i32, String> {
    let pid = crate::hook::walk::from_process()
        .cli_pid
        .ok_or("the Claude channel must be launched by Claude Code")?;
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
        .ok_or("no Claude config directory")?;
    let (tx, mut stdin) = mpsc::channel(16);
    std::thread::Builder::new()
        .name("channel-stdin".into())
        .spawn(move || {
            let mut input = io::stdin().lock();
            loop {
                let mut bytes = Vec::new();
                match input
                    .by_ref()
                    .take((MAX_LINE + 1) as u64)
                    .read_until(b'\n', &mut bytes)
                {
                    Ok(0) | Err(_) => break,
                    Ok(_) if bytes.len() > MAX_LINE => break,
                    Ok(_) => {
                        if tx
                            .blocking_send(serde_json::from_slice::<Value>(&bytes))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let result = rt.block_on(async {
        let mut protocol = Protocol::default();
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let mut refused = 0u32;
        let mut link: Option<(Reader<tokio::net::unix::OwnedReadHalf>, tokio::net::unix::OwnedWriteHalf)> = None;
        loop {
            tokio::select! {
                request = stdin.recv() => {
                    let Some(request) = request else { return Ok::<_, io::Error>(0) };
                    protocol.identify(current_identity(&config, pid));
                    match request {
                        Ok(request) => if let Some(response) = protocol.request(&request) { emit(&response)?; },
                        Err(_) => emit(&json!({"jsonrpc":"2.0", "id":null, "error":{"code":-32700,"message":"Parse error"}}))?,
                    }
                }
                _ = tick.tick() => {
                    protocol.identify(current_identity(&config, pid));
                    if link.is_none() {
                        if let Ok(Ok(mut stream)) = tokio::time::timeout(Duration::from_millis(250), UnixStream::connect(socket(data))).await {
                            if peer(&stream).is_err() {
                                refused += 1;
                            } else if write(&mut stream, &json!({"config_dir": config})).await.is_ok() {
                                refused = 0;
                                let (input, output) = stream.into_split();
                                link = Some((Reader::new(input), output));
                            }
                            if refused >= RETIRE_AFTER_REFUSALS {
                                eprintln!("openwalkietalkie: this channel helper belongs to an older OpenWalkieTalkie install; restart the Claude session to reconnect");
                                return Ok(0);
                            }
                        }
                    }
                    if let Some(probe) = protocol.probe()? { emit(&probe)?; }
                }
                request = async { match &mut link {
                    Some((input, _)) => input.next().await,
                    None => std::future::pending().await,
                }} => {
                    match request {
                        Ok(request) => {
                            protocol.identify(current_identity(&config, pid));
                            let actual = current_identity(&config, pid);
                            let delivery = if actual.is_none() || serde_json::from_value::<Identity>(request["identity"].clone()).ok() != actual {
                                "session_changed"
                            } else if protocol.accepts(&request, actual) {
                                emit(&event(request["text"].as_str().expect("validated"), "message"))?;
                                "sent"
                            } else { "unavailable" };
                            if let Some((_, output)) = &mut link {
                                if write(output, &json!({"delivery": delivery})).await.is_err() { link = None; }
                            }
                        }
                        Err(_) => link = None,
                    }
                }
            }
            if let Some((_, output)) = &mut link {
                if write(output, &protocol.status()).await.is_err() { link = None; }
            }
        }
    });
    rt.shutdown_timeout(Duration::from_millis(100));
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(session: &str) -> Identity {
        Identity {
            pid: 42,
            session: session.into(),
        }
    }
    fn call(name: &str, nonce: &str) -> Value {
        json!({"id": 1, "method": "tools/call", "params": {"name": name, "arguments": {"nonce": nonce}}})
    }

    #[test]
    fn initializing_mcp_does_not_prove_channels_are_enabled() {
        let mut p = Protocol::default();
        p.identify(Some(id("a")));
        let result = p.request(&json!({"id":1,"method":"initialize"})).unwrap();
        assert!(result["result"]["capabilities"]["experimental"]["claude/channel"].is_object());
        assert!(p.probe().unwrap().is_none());
        p.request(&json!({"method":"notifications/initialized"}));
        let probe = p.probe().unwrap().unwrap();
        let nonce = p.nonce.clone().unwrap();
        assert!(probe["params"]["content"]
            .as_str()
            .unwrap()
            .contains(&nonce));
        assert!(!p.ready);
        assert!(p.request(&call("acknowledge", "wrong")).unwrap()["result"]["isError"] == true);
        p.request(&call("acknowledge", &nonce));
        assert!(p.ready);
    }

    #[test]
    fn session_changes_invalidate_proofs_and_in_flight_messages() {
        let mut p = Protocol {
            initialized: true,
            ..Default::default()
        };
        p.identify(Some(id("a")));
        p.probe().unwrap();
        let old_nonce = p.nonce.clone().unwrap();
        p.request(&call("acknowledge", &old_nonce));
        let request = json!({"identity":id("a"),"text":"hello"});
        assert!(p.accepts(&request, Some(id("a"))));
        assert!(!p.accepts(&request, Some(id("b"))));
        p.identify(Some(id("b")));
        p.request(&call("acknowledge", &old_nonce));
        assert!(!p.ready);
        assert!(!p.accepts(&request, Some(id("b"))));
        p.probe().unwrap();
        assert_ne!(p.nonce.as_ref(), Some(&old_nonce));
    }
}
