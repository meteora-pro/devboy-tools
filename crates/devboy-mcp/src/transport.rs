//! Transport layer for MCP JSON-RPC communication.
//!
//! MCP uses newline-delimited JSON over stdin/stdout.

use std::io::{self, BufRead, Write};

use crate::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};

/// Message that can be received from the client.
#[derive(Debug)]
pub enum IncomingMessage {
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
}

/// Transport for reading/writing JSON-RPC messages.
pub struct StdioTransport {
    reader: Box<dyn BufRead + Send>,
    writer: Box<dyn Write + Send>,
}

impl StdioTransport {
    /// Create a transport using stdin/stdout.
    pub fn stdio() -> Self {
        Self {
            reader: Box::new(io::BufReader::new(io::stdin())),
            writer: Box::new(io::stdout()),
        }
    }

    /// Create a transport with custom reader/writer (for testing).
    #[cfg(test)]
    pub fn new(reader: Box<dyn BufRead + Send>, writer: Box<dyn Write + Send>) -> Self {
        Self { reader, writer }
    }

    /// Read a single JSON-RPC message from the transport.
    pub fn read_message(&mut self) -> io::Result<Option<IncomingMessage>> {
        let mut line = String::new();

        match self.reader.read_line(&mut line) {
            Ok(0) => Ok(None), // EOF
            Ok(_) => {
                let line = line.trim();
                if line.is_empty() {
                    return Ok(None);
                }

                tracing::debug!("Received: {}", line);

                // Try to parse as request first (has id field)
                if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(line) {
                    return Ok(Some(IncomingMessage::Request(request)));
                }

                // Try as notification (no id field)
                if let Ok(notification) = serde_json::from_str::<JsonRpcNotification>(line) {
                    return Ok(Some(IncomingMessage::Notification(notification)));
                }

                tracing::warn!("Failed to parse message: {}", line);
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Invalid JSON-RPC message: {}", line),
                ))
            }
            Err(e) => Err(e),
        }
    }

    /// Write a JSON-RPC response to the transport.
    pub fn write_response(&mut self, response: &JsonRpcResponse) -> io::Result<()> {
        let json = serde_json::to_string(response).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Serialization error: {}", e))
        })?;

        tracing::debug!("Sending: {}", json);

        writeln!(self.writer, "{}", json)?;
        self.writer.flush()
    }

    /// Write a JSON-RPC notification to the transport.
    pub fn write_notification(&mut self, notification: &JsonRpcNotification) -> io::Result<()> {
        let json = serde_json::to_string(notification).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Serialization error: {}", e))
        })?;

        tracing::debug!("Sending notification: {}", json);

        writeln!(self.writer, "{}", json)?;
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RequestId;
    use std::io::Cursor;

    #[test]
    fn test_read_request() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"test","params":{}}"#;
        let reader = Box::new(Cursor::new(format!("{}\n", input)));
        let writer = Box::new(Vec::new());

        let mut transport = StdioTransport::new(reader, writer);
        let msg = transport.read_message().unwrap();

        match msg {
            Some(IncomingMessage::Request(req)) => {
                assert_eq!(req.method, "test");
                assert_eq!(req.id, RequestId::Number(1));
            }
            _ => panic!("Expected request"),
        }
    }

    #[test]
    fn test_read_notification() {
        let input = r#"{"jsonrpc":"2.0","method":"initialized"}"#;
        let reader = Box::new(Cursor::new(format!("{}\n", input)));
        let writer = Box::new(Vec::new());

        let mut transport = StdioTransport::new(reader, writer);
        let msg = transport.read_message().unwrap();

        match msg {
            Some(IncomingMessage::Notification(notif)) => {
                assert_eq!(notif.method, "initialized");
            }
            _ => panic!("Expected notification"),
        }
    }

    #[test]
    fn test_write_response() {
        use std::sync::{Arc, Mutex};

        // Use Arc<Mutex<Vec>> to capture output
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let buffer_clone = buffer.clone();

        struct SharedWriter(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for SharedWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let reader = Box::new(Cursor::new(Vec::new()));
        let writer = Box::new(SharedWriter(buffer_clone));

        let mut transport = StdioTransport::new(reader, writer);

        let response = JsonRpcResponse::success(
            RequestId::Number(1),
            serde_json::json!({"test": true}),
        );

        transport.write_response(&response).unwrap();

        let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        assert!(output.contains("\"jsonrpc\":\"2.0\""));
        assert!(output.contains("\"id\":1"));
    }

    #[test]
    fn test_read_eof() {
        let reader = Box::new(Cursor::new(Vec::new()));
        let writer = Box::new(Vec::new());

        let mut transport = StdioTransport::new(reader, writer);
        let msg = transport.read_message().unwrap();

        assert!(msg.is_none());
    }
}
