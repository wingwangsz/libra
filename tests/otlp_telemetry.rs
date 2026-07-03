//! OTLP wire test (lore.md 1.7; runs only with `--features otlp`): a mock
//! collector captures /v1/traces requests from a real `libra` invocation and
//! asserts the allowlist — the command span arrives, and NO path/URL/token
//! material rides along.
//!
//! **Layer:** L1 — loopback networking only.

use std::{
    io::Read,
    sync::{Arc, Mutex},
};

#[test]
fn otlp_exports_the_error_span_on_failed_commands() {
    // The error exit path calls std::process::exit — the flush must still
    // run (explicit shutdown before exit; a scopeguard would be skipped).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let server = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 65536];
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .ok();
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.windows(4).any(|w| w == b"\r\n\r\n") && buffer.len() > 512 {
                    break;
                }
            }
            use std::io::Write;
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
            *sink.lock().expect("sink") = buffer;
        }
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    // `status` outside a repository fails with LBR-REPO-001.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(["status"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            format!("http://{addr}/v1/traces"),
        )
        .output()
        .expect("run libra status");
    assert!(!output.status.success(), "status outside a repo fails");
    server.join().expect("server thread");
    let body = captured.lock().expect("captured").clone();
    assert!(!body.is_empty(), "error-path export still flushed");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("libra.error_code"), "error code attr present");
    assert!(text.contains("LBR-"), "stable code value present");
}

#[test]
fn otlp_exports_only_the_vetted_span() {
    // Minimal blocking HTTP sink: accept one POST, capture the body.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let server = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 65536];
            // Read until the client finishes the request (best-effort).
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .ok();
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.windows(4).any(|w| w == b"\r\n\r\n") && buffer.len() > 512 {
                    break;
                }
            }
            use std::io::Write;
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
            *sink.lock().expect("sink") = buffer;
        }
    });

    // Run a real (otlp-featured) libra command against the mock collector.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(["init"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            format!("http://{addr}/v1/traces"),
        )
        .output()
        .expect("run libra init");
    assert!(output.status.success(), "init: {output:?}");

    server.join().expect("server thread");
    let body = captured.lock().expect("captured").clone();
    assert!(!body.is_empty(), "the collector received an export request");
    let text = String::from_utf8_lossy(&body);
    // The vetted span and service identity arrive (protobuf keeps ASCII
    // strings readable).
    assert!(text.contains("libra.command"), "span name/attr present");
    assert!(text.contains("init"), "canonical command name present");
    assert!(text.contains("libra"), "service.name present");
    // Allowlist: nothing resembling the repo path or HOME leaks.
    let leak = dir.path().to_string_lossy().to_string();
    assert!(
        !text.contains(leak.as_str()),
        "no filesystem paths in the export"
    );
}
