use kageha_contracts::decisions::*;
use kageha_contracts::execution::Control;
use kageha_models::{decisions::HttpDecisions, HttpModel, Provider};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::atomic::AtomicBool,
};
#[test]
fn real_wire_format_is_system_one_not_chat() {
    for protocol in ["clef", "system_one"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let clef = protocol == "clef";
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(payload.get("messages").is_none());
            assert_eq!(payload["questions"]["q"]["type"], "choice");
            assert_eq!(
                payload["model"],
                if clef {
                    "clef-flash"
                } else {
                    "typesafe/jev-1.14.0"
                }
            );
            let answer = json!({"answers":{"q":{"type":"choice","choice":"yes","probabilities":{"yes":0.97,"no":0.03}}},"usage":{"input_tokens":20,"output_tokens":0}});
            let body = if clef {
                json!({"success":true,"result":answer})
            } else {
                answer
            }
            .to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        let backend = HttpDecisions::new(
            HttpModel::new(vec![Provider {
                url: format!("http://{address}/typed"),
                key: String::new(),
                model: if clef {
                    "@cf/cloudflare/clef-flash"
                } else {
                    "typesafe/jev-1.14.0"
                }
                .into(),
                protocol: protocol.into(),
            }])
            .unwrap(),
        )
        .unwrap();
        let request = Request {
            state: json!({"fact":true}),
            questions: BTreeMap::from([(
                "q".into(),
                Question {
                    instructions: "Classify the explicit fact".into(),
                    criteria: BTreeMap::from([
                        ("yes".into(), "Explicit".into()),
                        ("no".into(), "Missing".into()),
                    ]),
                },
            )]),
        };
        let flag = AtomicBool::new(false);
        let result = backend
            .decide(
                &request,
                &Control {
                    cancelled: &flag,
                    deadline: None,
                },
            )
            .unwrap();
        assert_eq!(result.answers["q"].probability(), 0.97);
        worker.join().unwrap();
    }
}
#[test]
fn a_chat_model_cannot_be_registered_as_a_typed_decision_backend() {
    let model = HttpModel::new(vec![Provider {
        url: "http://127.0.0.1:1/chat".into(),
        key: String::new(),
        model: "ordinary-chat".into(),
        protocol: "openai_compat".into(),
    }])
    .unwrap();
    assert!(HttpDecisions::new(model).is_err());
}

#[test]
fn openai_decisions_wire_and_provider_confidence_are_preserved() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(&mut socket);
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                length = n.trim().parse().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        let p: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(p["model"], "gpt-6-luna");
        assert_eq!(p["input"][0]["content"][0]["type"], "input_json");
        assert_eq!(p["questions"][0]["name"], "q");
        assert!(p.get("messages").is_none());
        let body=json!({"model":"gpt-6-luna","answers":[{"type":"choice","name":"q","choice":"yes","confidence":0.73,"probabilities":[{"value":"yes","probability":0.97},{"value":"no","probability":0.03}]}],"usage":{"input_tokens":4,"output_tokens":0}}).to_string();
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
    });
    let backend = HttpDecisions::new(
        HttpModel::new(vec![Provider {
            url: format!("http://{address}/v1/decisions"),
            key: String::new(),
            model: "gpt-6-luna".into(),
            protocol: "openai_decisions".into(),
        }])
        .unwrap(),
    )
    .unwrap();
    let request = Request {
        state: json!({"test":true}),
        questions: BTreeMap::from([(
            "q".into(),
            Question {
                instructions: "Is the fact explicit?".into(),
                criteria: BTreeMap::from([
                    ("yes".into(), "Explicit".into()),
                    ("no".into(), "Absent".into()),
                ]),
            },
        )]),
    };
    let flag = AtomicBool::new(false);
    let result = backend
        .decide(
            &request,
            &Control {
                cancelled: &flag,
                deadline: None,
            },
        )
        .unwrap();
    assert_eq!(result.answers["q"].confidence, Some(0.73));
    assert_eq!(result.answers["q"].probability(), 0.97);
    server.join().unwrap();
}
