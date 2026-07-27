//! Runs one debug session over the tuni-core client and prints what happens.
//! A repro, not a test: hand it an adapter command and a program, and a
//! breakpoint goes on the given line.
//!
//! ```sh
//! cargo run -p tuni-core --example dap_probe -- 'python3 -m debugpy.adapter' demo.py 2
//! ```

use std::path::Path;
use std::sync::mpsc;

use tuni_core::dap;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let (Some(adapter), Some(program)) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: dap_probe '<adapter command>' <program> [line]");
        std::process::exit(2);
    };
    let line: usize = arguments
        .next()
        .and_then(|line| line.parse().ok())
        .unwrap_or(1);
    let program = Path::new(&program).canonicalize().unwrap();
    let command: Vec<&str> = adapter.split_whitespace().collect();
    let debugger = if program.extension().is_some_and(|e| e == "py") {
        dap::debugger_for_language("python")
    } else {
        dap::debugger_for_language("rust")
    }
    .unwrap();

    let cwd = program.parent().unwrap().to_path_buf();
    let mut client = dap::Client::spawn(&command, &cwd).unwrap();
    let (sender, receiver) = mpsc::channel();
    client.start_reader(move |event| {
        let _ = sender.send(event);
    });
    client.initialize(debugger.id);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut thread = None;
    while std::time::Instant::now() < deadline {
        let Ok(event) = receiver.recv_timeout(std::time::Duration::from_secs(1)) else {
            continue;
        };
        let message = match event {
            dap::Event::Exited => {
                eprintln!("adapter exited");
                break;
            }
            dap::Event::Message(message) => message,
        };
        match dap::parse_incoming(&message) {
            dap::Incoming::Response {
                request_seq,
                command,
                success,
                message: explanation,
                body,
            } => {
                let asked = client.take_pending(request_seq);
                eprintln!("response {command} success={success} (was {asked:?})");
                if !success {
                    eprintln!("  because: {explanation:?}");
                    break;
                }
                match command.as_str() {
                    "initialize" => {
                        client.launch(dap::launch_arguments(debugger, &program, &[], &cwd));
                    }
                    "stackTrace" => {
                        let stack = dap::parse_stack(&body);
                        for frame in &stack {
                            eprintln!("  frame {} at {:?}:{}", frame.name, frame.path, frame.line);
                        }
                        if let Some(top) = stack.first() {
                            client.scopes(top.id);
                        }
                    }
                    "scopes" => {
                        if let Some((name, reference)) = dap::parse_scopes(&body).into_iter().next()
                        {
                            eprintln!("  scope {name} -> {reference}");
                            client.variables(reference);
                        }
                    }
                    "variables" => {
                        for variable in dap::parse_variables(&body) {
                            eprintln!("  {} = {}", variable.name, variable.value);
                        }
                        if let Some(thread) = thread {
                            client.resume(thread);
                        }
                    }
                    _ => {}
                }
            }
            dap::Incoming::Initialized => {
                eprintln!("initialized; breakpoint on line {line}");
                client.set_breakpoints(&program, &[line]);
                client.configuration_done();
            }
            dap::Incoming::Stopped {
                thread: stopped,
                reason,
            } => {
                eprintln!("stopped: {reason} on {stopped:?}");
                thread = stopped;
                if let Some(thread) = stopped {
                    client.stack_trace(thread);
                } else {
                    client.threads();
                }
            }
            dap::Incoming::Output { text, category } => {
                eprint!("[{category}] {text}");
            }
            dap::Incoming::ProgramExited { code } => eprintln!("program exited: {code}"),
            dap::Incoming::Terminated => {
                eprintln!("terminated");
                break;
            }
            dap::Incoming::Continued | dap::Incoming::Other => {}
        }
    }
}
