use horde::{
    config::{Decision, Settings},
    decision::{ChoiceQuestion, DecisionRequest, NoulQuestion, Question, ScoreQuestion},
    store::Store,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub static CONFIG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct OperatorConfig {
    previous: Option<std::ffi::OsString>,
    pub directory: tempfile::TempDir,
}

impl OperatorConfig {
    pub fn install(decision: &Decision) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config_dir = directory.path().join("horde");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            toml::to_string(&Settings {
                decision: decision.clone(),
                ..Settings::default()
            })
            .unwrap(),
        )
        .unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", directory.path()) };
        Self {
            previous,
            directory,
        }
    }

    pub fn rewrite(&self, decision: &Decision) {
        std::fs::write(
            self.directory.path().join("horde/config.toml"),
            toml::to_string(&Settings {
                decision: decision.clone(),
                ..Settings::default()
            })
            .unwrap(),
        )
        .unwrap();
    }
}

impl Drop for OperatorConfig {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            unsafe { std::env::set_var("XDG_CONFIG_HOME", previous) };
        } else {
            unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        }
    }
}

pub fn request() -> DecisionRequest {
    DecisionRequest {
        model: "jev-1.13.0".into(),
        state: json!({"objective":"A small Rust change with complete acceptance criteria."}),
        questions: vec![
            Question::Choice(ChoiceQuestion {
                id: "route".into(),
                question: "Which eligible capability should run this work?".into(),
                options: vec![
                    "local/codex".into(),
                    "local/claude".into(),
                    "abstain".into(),
                ],
            }),
            Question::Score(ScoreQuestion {
                id: "difficulty".into(),
                question: "How difficult is the work?".into(),
                legend: vec!["routine".into(), "moderate".into(), "complex".into()],
            }),
            Question::Noul(NoulQuestion {
                id: "security".into(),
                question: "Does this change touch a security-sensitive boundary?".into(),
            }),
        ],
    }
}

pub async fn server(
    responses: Vec<(&'static str, &'static str, u64)>,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for (status, body, delay_ms) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![];
            let mut buffer = [0_u8; 4096];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&bytes[..header_end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .or_else(|| line.strip_prefix("Content-Length: "))?
                                .parse::<usize>()
                                .ok()
                        })
                        .unwrap_or(0);
                    if bytes.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            let header_end = bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();
            send.send(bytes[header_end + 4..].to_vec()).unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let retry = if status == "429 Too Many Requests" {
                "Retry-After: 0\r\n"
            } else {
                ""
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{retry}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{address}"), receive)
}

pub fn response_json() -> &'static str {
    r#"{"model":"jev-1.13.0","answers":{"route":{"type":"choice","choice":"local/codex","confidence":0.7,"probabilities":{"local/codex":0.7,"local/claude":0.2,"abstain":0.1}},"difficulty":{"type":"score","score":1.3,"confidence":0.6,"legend":["routine","moderate","complex"],"probabilities":{"0":0.1,"1":0.5,"2":0.4}},"security":{"type":"noul","noul":0.25}},"usage":{"input_tokens":21,"output_tokens":3}}"#
}

pub fn shadow_response() -> &'static str {
    r#"{"model":"jev-1.13.0","answers":{"route":{"type":"choice","choice":"local/simulated","confidence":0.8,"probabilities":{"local/simulated":0.8,"abstain":0.2}},"difficulty":{"type":"score","score":1.0,"confidence":0.7,"legend":["routine","moderate","complex"],"probabilities":{"0":0.2,"1":0.6,"2":0.2}},"security":{"type":"noul","noul":0.1},"insufficient_evidence":{"type":"noul","noul":0.1}},"usage":{"input_tokens":12,"output_tokens":4}}"#
}

pub async fn drain(queue: &mut horde::decision::shadow::Queue, db: &Store) {
    for _ in 0..100 {
        queue.tick(db).await.unwrap();
        if queue.is_idle() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("decision queue did not drain");
}
