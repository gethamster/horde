//! Tool-free, advisory decisions. A decision can record a proposal, but it has
//! no authority to change an executor, consume a worker attempt, or perform an
//! external mutation.
pub mod review;
pub mod shadow;
pub mod store;
pub mod typesafe;
pub use typesafe::DecisionHttpClient;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_QUESTIONS: usize = 16;
pub const MAX_REQUEST_BYTES: usize = 60 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 30 * 1024;
const EPSILON: f64 = 1e-6;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    pub model: String,
    pub state: Value,
    pub questions: Vec<Question>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    Choice(ChoiceQuestion),
    Score(ScoreQuestion),
    Noul(NoulQuestion),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceQuestion {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreQuestion {
    pub id: String,
    pub question: String,
    pub legend: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoulQuestion {
    pub id: String,
    pub question: String,
}

impl Question {
    fn id(&self) -> &str {
        match self {
            Self::Choice(value) => &value.id,
            Self::Score(value) => &value.id,
            Self::Noul(value) => &value.id,
        }
    }
    fn prompt(&self) -> &str {
        match self {
            Self::Choice(value) => &value.question,
            Self::Score(value) => &value.question,
            Self::Noul(value) => &value.question,
        }
    }
}

fn bounded_label(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value == value.trim()
        && !value.chars().any(char::is_control)
}

impl DecisionRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.model.is_empty()
                && self.model.len() <= 128
                && self.model.bytes().all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'_' | b'.' | b'/')),
            "invalid decision model identifier"
        );
        ensure!(
            !self.questions.is_empty() && self.questions.len() <= MAX_QUESTIONS,
            "decision request must contain 1..={MAX_QUESTIONS} questions"
        );
        let mut ids = BTreeSet::new();
        let mut longest = 0;
        for question in &self.questions {
            ensure!(bounded_label(question.id(), 64), "invalid question id");
            ensure!(ids.insert(question.id()), "duplicate question id");
            ensure!(
                !question.prompt().trim().is_empty() && question.prompt().len() <= 8 * 1024,
                "invalid question text"
            );
            longest = longest.max(question.prompt().len());
            let values = match question {
                Question::Choice(value) => &value.options,
                Question::Score(value) => &value.legend,
                Question::Noul(_) => continue,
            };
            ensure!(
                (2..=64).contains(&values.len()),
                "choice and score questions need 2..=64 labels"
            );
            ensure!(
                values.iter().all(|value| bounded_label(value, 256)),
                "invalid question label"
            );
            ensure!(
                values.iter().collect::<BTreeSet<_>>().len() == values.len(),
                "duplicate question label"
            );
        }
        ensure!(
            matches!(
                &self.state,
                Value::String(_) | Value::Object(_) | Value::Array(_)
            ),
            "decision state must be a string, object, or array"
        );
        let state_bytes = serde_json::to_vec(&self.state)?;
        ensure!(
            state_bytes.len() + longest <= MAX_CONTEXT_BYTES,
            "decision state and longest question exceed the context limit"
        );
        ensure!(
            serde_json::to_vec(&wire_request(self))?.len() <= MAX_REQUEST_BYTES,
            "serialized decision request exceeds the request limit"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DecisionResponse {
    pub answers: Vec<Answer>,
    pub usage: DecisionUsage,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Choice {
        id: String,
        answer: String,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        id: String,
        score: f64,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    Noul {
        id: String,
        noul: f64,
        confidence: Option<f64>,
    },
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DecisionUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

fn probability(value: &Value, name: &str) -> Result<f64> {
    let value = value
        .as_f64()
        .with_context(|| format!("{name} must be numeric"))?;
    ensure!(
        value.is_finite() && (0.0..=1.0).contains(&value),
        "{name} must be finite and between 0 and 1"
    );
    Ok(value)
}

fn distribution(value: &Value, labels: &[String]) -> Result<BTreeMap<String, f64>> {
    let object = value
        .as_object()
        .context("probabilities must be an object")?;
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = labels.iter().map(String::as_str).collect::<BTreeSet<_>>();
    ensure!(
        actual == expected,
        "probability keys do not match the question labels"
    );
    let result = object
        .iter()
        .map(|(key, value)| Ok((key.clone(), probability(value, "probability")?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    ensure!(
        (result.values().sum::<f64>() - 1.0).abs() <= EPSILON,
        "probabilities must sum to one"
    );
    Ok(result)
}

pub fn wire_request(request: &DecisionRequest) -> Value {
    let questions = request.questions.iter().map(|question| {
        let wire = match question {
            Question::Choice(value) => serde_json::json!({
                "type":"choice","instructions":value.question,
                "criteria":value.options.iter().map(|option|(option.clone(),Value::Null)).collect::<serde_json::Map<_,_>>()
            }),
            Question::Score(value) => serde_json::json!({"type":"score","instructions":value.question,"criteria":value.legend}),
            Question::Noul(value) => serde_json::json!({"type":"noul","instructions":value.question}),
        };
        (question.id().to_owned(), wire)
    }).collect::<serde_json::Map<_,_>>();
    serde_json::json!({"model":request.model,"state":request.state,"questions":questions})
}

fn score_legend_matches(value: &Value, expected: &[String]) -> bool {
    match value {
        Value::Array(labels) => {
            labels.len() == expected.len()
                && labels
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual.as_str() == Some(expected.as_str()))
        }
        Value::Object(labels) => {
            labels.len() == expected.len()
                && expected.iter().enumerate().all(|(index, expected)| {
                    labels.get(&index.to_string()).and_then(Value::as_str)
                        == Some(expected.as_str())
                })
        }
        _ => false,
    }
}

pub fn validate_response(request: &DecisionRequest, value: &Value) -> Result<DecisionResponse> {
    request.validate()?;
    ensure!(
        value["model"] == request.model,
        "decision response model mismatch"
    );
    let by_id = value["answers"]
        .as_object()
        .context("decision response needs an answers object")?;
    ensure!(
        by_id.len() == request.questions.len(),
        "decision answer count mismatch"
    );
    let actual = by_id.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = request
        .questions
        .iter()
        .map(Question::id)
        .collect::<BTreeSet<_>>();
    ensure!(
        actual == expected,
        "decision answer ids do not match the request"
    );
    let mut answers = vec![];
    for question in &request.questions {
        let row = by_id.get(question.id()).context("answer id mismatch")?;
        let kind = match question {
            Question::Choice(_) => "choice",
            Question::Score(_) => "score",
            Question::Noul(_) => "noul",
        };
        ensure!(row["type"] == kind, "answer type mismatch");
        match question {
            Question::Choice(question) => {
                let probabilities = distribution(&row["probabilities"], &question.options)?;
                let answer = row["choice"].as_str().context("choice answer missing")?;
                ensure!(
                    question.options.iter().any(|option| option == answer),
                    "choice answer was not offered"
                );
                let selected = probabilities[answer];
                ensure!(
                    probabilities
                        .values()
                        .all(|value| *value <= selected + EPSILON),
                    "choice answer is not an argmax"
                );
                let confidence = probability(&row["confidence"], "confidence")?;
                answers.push(Answer::Choice {
                    id: question.id.clone(),
                    answer: answer.into(),
                    confidence,
                    probabilities,
                });
            }
            Question::Score(question) => {
                ensure!(
                    score_legend_matches(&row["legend"], &question.legend),
                    "score legend does not match the request"
                );
                let probability_keys = (0..question.legend.len())
                    .map(|index| index.to_string())
                    .collect::<Vec<_>>();
                let probabilities = distribution(&row["probabilities"], &probability_keys)?;
                let score = row["score"].as_f64().context("score answer missing")?;
                ensure!(score.is_finite(), "score must be finite");
                let expected = question
                    .legend
                    .iter()
                    .enumerate()
                    .map(|(index, _)| index as f64 * probabilities[&index.to_string()])
                    .sum::<f64>();
                ensure!(
                    (score - expected).abs() <= EPSILON,
                    "score does not equal the weighted legend index"
                );
                let confidence = probability(&row["confidence"], "confidence")?;
                answers.push(Answer::Score {
                    id: question.id.clone(),
                    score,
                    confidence,
                    probabilities,
                });
            }
            Question::Noul(question) => {
                let noul = probability(&row["noul"], "noul")?;
                let confidence = row
                    .get("confidence")
                    .filter(|value| !value.is_null())
                    .map(|value| probability(value, "confidence"))
                    .transpose()?;
                answers.push(Answer::Noul {
                    id: question.id.clone(),
                    noul,
                    confidence,
                });
            }
        }
    }
    let usage = value.get("usage").unwrap_or(&Value::Null);
    let token = |name: &str| -> Result<Option<u64>> {
        match usage.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .with_context(|| format!("usage {name} must be nonnegative")),
        }
    };
    Ok(DecisionResponse {
        answers,
        usage: DecisionUsage {
            input_tokens: token("input_tokens")?,
            output_tokens: token("output_tokens")?,
        },
    })
}

#[tonic::async_trait]
pub trait DecisionBackend: Send + Sync {
    async fn decide(&self, request: &DecisionRequest) -> Result<DecisionResponse>;
}

pub fn safe_error(error: &anyhow::Error) -> String {
    let message = error.to_string();
    if message.chars().count() <= 512 {
        message
    } else {
        format!("{}…", message.chars().take(511).collect::<String>())
    }
}

pub fn ensure_label(value: &str, name: &str) -> Result<()> {
    if !bounded_label(value, 256) {
        bail!("invalid {name}");
    }
    Ok(())
}
