//! Smoke test for provider streaming.
//! Run with: cargo test --test provider_smoke -- --ignored --nocapture
//! Requires GEMINI_API_KEY (or OPENAI_API_KEY, etc.) to be set.

use futures::StreamExt;
use genai::Client;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent};

#[tokio::test]
#[ignore] // Requires API key and network access
async fn test_gemini_streaming() {
    let client = Client::default();

    let chat_req = ChatRequest::new(vec![ChatMessage::user("Say hello in exactly 5 words.")]);

    let options = ChatOptions::default().with_capture_usage(true);

    let res = client
        .exec_chat_stream("gemini-2.5-flash", chat_req, Some(&options))
        .await
        .expect("Failed to start stream");

    let mut stream = res.stream;
    let mut full_text = String::new();
    let mut got_start = false;
    let mut got_end = false;

    while let Some(event) = stream.next().await {
        match event.expect("Stream event error") {
            ChatStreamEvent::Start => {
                got_start = true;
                println!("[START]");
            }
            ChatStreamEvent::Chunk(chunk) => {
                print!("{}", chunk.content);
                full_text.push_str(&chunk.content);
            }
            ChatStreamEvent::End(end) => {
                got_end = true;
                println!("\n[END]");
                if let Some(usage) = end.captured_usage {
                    println!(
                        "Usage: prompt={:?}, completion={:?}, total={:?}",
                        usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                    );
                }
            }
            _ => {}
        }
    }

    println!("Full response: {}", full_text);

    assert!(got_start, "Should have received Start event");
    assert!(got_end, "Should have received End event");
    assert!(!full_text.is_empty(), "Should have received some text");
}
