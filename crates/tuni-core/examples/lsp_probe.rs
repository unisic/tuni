//! Talks to rust-analyzer over the tuni-core client and prints what happens.
//! A repro, not a test: run it against a crate with an error in it.

use std::path::Path;
use std::sync::mpsc;

use tuni_core::lsp;

fn main() {
    let file = std::env::args().nth(1).expect("usage: lsp_probe <file.rs>");
    let path = Path::new(&file).canonicalize().unwrap();
    let language = lsp::language_for_path(&path).expect("no language");
    let command = lsp::available_command(language).expect("no server");
    let root = lsp::find_root(&path, language);
    eprintln!("server {command:?} root {}", root.display());

    let mut connection = lsp::Connection::spawn(command, &root).unwrap();
    let (sender, receiver) = mpsc::channel();
    connection.start_reader(move |event| {
        let _ = sender.send(event);
    });
    connection.initialize(&root);

    let text = std::fs::read_to_string(&path).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        let Ok(event) = receiver.recv_timeout(std::time::Duration::from_secs(1)) else {
            continue;
        };
        match event {
            lsp::Event::Exited => {
                eprintln!("exited");
                break;
            }
            lsp::Event::Message(message) => match lsp::parse_incoming(&message) {
                lsp::Incoming::Response { id, error, .. } => {
                    eprintln!("response id={id} error={error:?}");
                    if let Some(lsp::Pending::Initialize) = connection.take_pending(id) {
                        connection
                            .finish_initialize(message.get("result").unwrap_or(&lsp::Value::Null));
                        connection.open(&path, language.id, &text);
                        eprintln!("opened {}", path.display());
                    }
                }
                lsp::Incoming::Diagnostics { uri, diagnostics } => {
                    eprintln!("diagnostics for {uri}: {}", diagnostics.len());
                    for diagnostic in &diagnostics {
                        eprintln!(
                            "  {:?} {:?} {}",
                            diagnostic.severity, diagnostic.start, diagnostic.message
                        );
                    }
                    if !diagnostics.is_empty() {
                        return;
                    }
                }
                lsp::Incoming::Message { text } => eprintln!("message: {text}"),
                lsp::Incoming::Other => {
                    if let Some(method) = message.get("method").and_then(|m| m.as_str()) {
                        eprintln!("note: {method}");
                    }
                }
            },
        }
    }
}
