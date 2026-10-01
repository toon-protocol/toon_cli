//! A payment-oblivious app on loopback: what a connector delivers a route to. It
//! answers every request 200 with the body `ok`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;

pub struct StubApp {
    url: String,
}

impl StubApp {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stub app");
        let url = format!("http://{}/", listener.local_addr().expect("its address"));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                thread::spawn(move || {
                    let mut reader = BufReader::new(stream);
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some(value) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0; length];
                    let _ = reader.read_exact(&mut body);
                    let _ = reader.get_mut().write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    );
                });
            }
        });
        Self { url }
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}
