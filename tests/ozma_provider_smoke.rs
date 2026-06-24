//! Smoke test for ozma's Provider wrapper (config → client → streaming).
//! Run with: cargo test --test ozma_provider_smoke -- --ignored --nocapture

use tokio::sync::mpsc;

// We need to test the actual ozma provider module
// Since provider.rs is in the binary crate, we'll test the integration
// by using genai directly but mimicking what Provider does.

#[tokio::test]
#[ignore]
async fn test_ozma_provider_bridge() {
    use futures::StreamExt;
    use genai::Client;
    use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent};

    // Mimic what Provider::send_streaming does
    let client = Client::default();
    let chat_req = ChatRequest::new(vec![ChatMessage::user(
        "Say 'test passed' in exactly two words.",
    )]);
    let options = ChatOptions::default().with_capture_usage(true);

    // Create the action channel (like our bridge pattern)
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let tx_clone = tx.clone();
    tokio::spawn(async move {
        let res = client
            .exec_chat_stream("gemini-2.5-flash", chat_req, Some(&options))
            .await
            .expect("Failed to start stream");
        let mut stream = res.stream;

        while let Some(event) = stream.next().await {
            match event {
                Ok(ChatStreamEvent::Chunk(chunk)) => {
                    tx_clone.send(format!("CHUNK:{}", chunk.content)).unwrap();
                }
                Ok(ChatStreamEvent::End(_)) => {
                    tx_clone.send("DONE".to_string()).unwrap();
                }
                Ok(ChatStreamEvent::Start) => {
                    tx_clone.send("START".to_string()).unwrap();
                }
                Err(e) => {
                    tx_clone.send(format!("ERROR:{}", e)).unwrap();
                    return;
                }
                _ => {}
            }
        }
    });

    // Receive events like the TUI main loop would
    let mut got_start = false;
    let mut got_done = false;
    let mut full_text = String::new();

    while let Some(msg) = rx.recv().await {
        println!("Received: {}", msg);
        if msg == "START" {
            got_start = true;
        } else if msg == "DONE" {
            got_done = true;
            break;
        } else if let Some(chunk) = msg.strip_prefix("CHUNK:") {
            full_text.push_str(chunk);
        } else if msg.starts_with("ERROR:") {
            panic!("Stream error: {}", msg);
        }
    }

    println!("Full text: {}", full_text);
    assert!(got_start);
    assert!(got_done);
    assert!(!full_text.is_empty());
}
