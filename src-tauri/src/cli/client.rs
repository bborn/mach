//! The command line's half of the conversation: find the door, knock, and turn
//! whatever comes back into something with an exit code on it.
//!
//! # Why the HTTP is written out longhand
//!
//! `reqwest` is already in the tree, and it would be one line. It is also
//! async, which means a Tokio runtime spun up and torn down for a single
//! request to a socket on this machine — and a command-line tool that a script
//! calls in a loop pays that on every invocation. The request here is one POST
//! to `127.0.0.1` with two headers and a JSON body, and the response is a
//! status line, a `Content-Length` and a body. Forty lines of `TcpStream` is
//! the smaller thing, and it starts instantly.
//!
//! It also makes one property obvious that would otherwise be buried in a
//! client's defaults: **no `Origin` header is sent, ever.** The door refuses
//! anything carrying one, and it is worth being able to see that the CLI has
//! nothing to strip.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::endpoint::{self, Endpoint};
use super::error::CliError;
use super::protocol::DoorRequest;

/// How long to wait on a door that accepted the connection but has not
/// answered.
///
/// A tool call can reach Google, and Google can be slow; this has to be longer
/// than the app's own retry budget or the CLI would report a timeout for a
/// command that then succeeded. Two minutes is far past any of them.
const TIMEOUT: Duration = Duration::from_secs(120);

/// A located, live-looking door.
pub struct Client {
    endpoint: Endpoint,
    data_dir: PathBuf,
}

impl Client {
    /// Find the door for this instance, or say why there is not one.
    ///
    /// The three failures are told apart on purpose, because they need three
    /// different things done about them: no file at all means the app has never
    /// run here; a file whose writer is gone means it crashed and left the token
    /// behind; and a file that will not parse means a version mismatch or a
    /// half-written file.
    pub fn locate(data_dir: PathBuf) -> Result<Client, CliError> {
        let path = endpoint::path_in(&data_dir);
        if !path.exists() {
            return Err(CliError::new(
                "notRunning",
                format!(
                    "Mach is not running — there is no door at {}. Writes need the app; \
                     reads do not.",
                    path.display()
                ),
            ));
        }
        let endpoint = endpoint::read(&data_dir).map_err(|e| CliError::new("notRunning", e))?;
        if !endpoint.writer_is_alive() {
            return Err(CliError::new(
                "notRunning",
                format!(
                    "Mach is not running — process {} wrote {} and is gone. It was killed \
                     without shutting down; the file is stale and can be deleted.",
                    endpoint.pid,
                    path.display()
                ),
            ));
        }
        Ok(Client { endpoint, data_dir })
    }

    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// One request, one answer.
    pub fn ask(&self, request: &DoorRequest) -> Result<Value, CliError> {
        let body = serde_json::to_string(request).map_err(|e| CliError::new("door", e.to_string()))?;
        let raw = self.post(&body)?;
        let answer: Value = serde_json::from_str(&raw)
            .map_err(|e| CliError::new("door", format!("the door said something unreadable: {e}")))?;

        if answer.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(answer);
        }
        let error = answer.get("error");
        let kind = error
            .and_then(|e| e.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("refused");
        let message = error
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("the door refused, and did not say why");
        Err(CliError::new(kind, message))
    }

    fn post(&self, body: &str) -> Result<String, CliError> {
        let address = format!("127.0.0.1:{}", self.endpoint.port);
        let mut stream = TcpStream::connect(&address).map_err(|e| {
            CliError::new(
                "notRunning",
                format!(
                    "nothing answered at {address} ({e}). Mach wrote the door file and then \
                     stopped; writes need the app."
                ),
            )
        })?;
        stream.set_read_timeout(Some(TIMEOUT)).ok();
        stream.set_write_timeout(Some(TIMEOUT)).ok();

        // `Host` has to be loopback and there is deliberately no `Origin`; the
        // door checks both. `Connection: close` so the body ends at EOF and
        // there is no keep-alive state to reason about for a one-shot process.
        let request = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Authorization: Bearer {token}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            path = super::door::PATH,
            port = self.endpoint.port,
            token = self.endpoint.token,
            len = body.len(),
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| CliError::new("door", format!("could not send to the door: {e}")))?;

        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|e| CliError::new("door", format!("the door stopped mid-answer: {e}")))?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        let (head, body) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| CliError::new("door", "the door's answer had no body"))?;
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);

        match status {
            200 => Ok(body.to_string()),
            // The token in the file is not the token the app is holding, which
            // means the file is from an app that has since restarted.
            401 => Err(CliError::new(
                "door",
                format!(
                    "the door refused this token. {} is stale — Mach has restarted since it \
                     was written.",
                    endpoint::path_in(&self.data_dir).display()
                ),
            )),
            403 => Err(CliError::new(
                "door",
                "the door refused the request outright. It only answers loopback requests \
                 with no Origin header.",
            )),
            other => Err(CliError::new(
                "door",
                format!("the door answered {other}: {}", body.trim()),
            )),
        }
    }
}
