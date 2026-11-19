//! `McpService::serve` runs tool calls concurrently: a call parked on a consent card must not
//! hold up any other request.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::{BufReader, Read, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use csm_engine::{Caller, Event};
use csm_mcp::{LocalBackend, McpOptions, McpService};
use serde_json::{Value, json};

/// A reader fed from a channel; end of input when the sender is dropped.
struct ChanRead(mpsc::Receiver<Vec<u8>>, Vec<u8>);

impl Read for ChanRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.1.is_empty() {
            match self.0.recv() {
                Ok(v) => self.1 = v,
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.1.len());
        buf[..n].copy_from_slice(&self.1[..n]);
        self.1.drain(..n);
        Ok(n)
    }
}

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn messages(&self) -> Vec<Value> {
        let text = String::from_utf8(self.0.lock().unwrap().clone()).unwrap();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn reply(&self, id: u64) -> Option<Value> {
        self.messages().into_iter().find(|m| m["id"] == id)
    }

    fn wait_reply(&self, id: u64, within: Duration) -> Option<Value> {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if let Some(r) = self.reply(id) {
                return Some(r);
            }
            thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

fn send(tx: &mpsc::Sender<Vec<u8>>, id: u64, method: &str, params: Value) {
    let mut line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string().into_bytes();
    line.push(b'\n');
    tx.send(line).unwrap();
}

#[test]
fn a_call_parked_on_consent_does_not_block_other_calls() {
    let engine = common::engine();
    let events = engine.subscribe();
    let (tx, rx) = mpsc::channel();
    let out = Shared::default();
    let (backend, writer) = (LocalBackend::new(engine.clone()), out.clone());
    let server = thread::spawn(move || {
        let mut service = McpService::new(backend, McpOptions::default());
        service.serve(BufReader::new(ChanRead(rx, Vec::new())), writer).unwrap();
    });

    send(&tx, 1, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}));
    assert!(out.wait_reply(1, Duration::from_secs(5)).is_some());

    // 2: needs consent, so it parks. 3 and 4 must be answered while it waits.
    send(&tx, 2, "tools/call", json!({"name": "fake_send", "arguments": {"text": "hi"}}));
    let request = loop {
        if let Event::ConsentRequested { request } = events.recv_timeout(Duration::from_secs(5)).expect("a consent request") {
            break request;
        }
    };
    send(&tx, 3, "tools/call", json!({"name": "fake_list", "arguments": {}}));
    send(&tx, 4, "tools/list", json!({}));
    let list = out.wait_reply(3, Duration::from_secs(5)).expect("fake_list answered while fake_send waits");
    assert_eq!(list["result"]["isError"], Value::Null);
    assert!(out.wait_reply(4, Duration::from_secs(5)).is_some(), "tools/list answered too");
    assert!(out.reply(2).is_none(), "the parked call has not answered yet");

    engine.call(Caller::Ui, "consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"})).unwrap();
    let sent = out.wait_reply(2, Duration::from_secs(5)).expect("the parked call finishes once approved");
    assert!(sent["result"]["content"][0]["text"].as_str().unwrap().contains("Sent"));

    drop(tx);
    server.join().unwrap();
}

#[test]
fn bad_arguments_and_unknown_tools_still_get_their_errors() {
    let engine = common::engine();
    let (tx, rx) = mpsc::channel();
    let out = Shared::default();
    let (backend, writer) = (LocalBackend::new(engine), out.clone());
    let server = thread::spawn(move || {
        McpService::new(backend, McpOptions::default()).serve(BufReader::new(ChanRead(rx, Vec::new())), writer).unwrap();
    });
    send(&tx, 1, "tools/call", json!({"name": "fake_echo", "arguments": {"nope": 1}}));
    send(&tx, 2, "tools/call", json!({"name": "no_such_tool", "arguments": {}}));
    send(&tx, 3, "tools/call", json!({"name": "fake_branch", "arguments": {}}));
    for id in 1..=3 {
        out.wait_reply(id, Duration::from_secs(5)).unwrap_or_else(|| panic!("no reply to {id}"));
    }
    assert!(out.reply(1).unwrap().get("error").is_some());
    assert!(out.reply(2).unwrap().get("error").is_some());
    assert!(out.reply(3).unwrap()["result"]["content"][0]["text"].as_str().unwrap().contains("toolset_enable"));
    drop(tx);
    server.join().unwrap();
}
