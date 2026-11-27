//! Shared questionnaire schema and the in-memory rendezvous for blocking asks.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const INSTRUCTIONS: &str = "\nResolve ambiguous implementation details with the user before implementing by calling `tools.ask_user` when the decision blocks progress. Use `tools.ask_user_async` if independent work can continue while the user answers.\n";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    questions: Vec<RawQuestion>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQuestion {
    id: String,
    question: String,
    kind: Kind,
    // Ignored for text, including values that are not arrays.
    options: Option<serde_json::Value>,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Select,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionKind {
    Select(Vec<String>),
    Text,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub question: String,
    pub kind: QuestionKind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    questions: Vec<Question>,
}
impl Request {
    pub fn parse(input: &str) -> Result<Self> {
        ensure!(
            input.len() <= MAX_REQUEST_BYTES,
            "Questionnaire request exceeds 1 MiB; ask fewer or shorter questions"
        );
        let input: Input =
            serde_json::from_str(input).context("Invalid questionnaire arguments")?;
        ensure!(
            (1..=24).contains(&input.questions.len()),
            "Provide 1–24 questions"
        );
        let mut ids = std::collections::HashSet::new();
        let mut questions = Vec::with_capacity(input.questions.len());
        for q in input.questions {
            ensure!(
                !q.id.trim().is_empty() && q.id.len() <= 128,
                "Question IDs must be nonblank and at most 128 UTF-8 bytes"
            );
            ensure!(ids.insert(q.id.clone()), "Duplicate question ID: {}", q.id);
            ensure!(
                !q.question.trim().is_empty(),
                "Question {} needs nonblank question text",
                q.id
            );
            let kind = match q.kind {
                Kind::Text => QuestionKind::Text,
                Kind::Select => {
                    let options: Vec<String> = serde_json::from_value(
                        q.options.context("Select questions require options")?,
                    )
                    .context("Options must be an array of strings")?;
                    ensure!(
                        (1..=20).contains(&options.len())
                            && options.iter().all(|s| !s.trim().is_empty()),
                        "Select questions require 1–20 nonblank options"
                    );
                    QuestionKind::Select(options)
                }
            };
            questions.push(Question {
                id: q.id,
                question: q.question,
                kind,
            });
        }
        Ok(Self { questions })
    }
    pub fn questions(&self) -> &[Question] {
        &self.questions
    }
    fn validate_answers(&self, answers: &[String]) -> Result<()> {
        ensure!(
            answers.len() == self.questions.len(),
            "Answer every question before submitting"
        );
        ensure!(
            answers.iter().all(|s| !s.trim().is_empty()),
            "Answers must not be blank"
        );
        Ok(())
    }
    fn answer(&self, answers: Vec<String>) -> Result<String> {
        self.validate_answers(&answers)?;
        #[derive(Serialize)]
        struct Answer<'a> {
            id: &'a str,
            answer: String,
        }
        #[derive(Serialize)]
        struct Answers<'a> {
            answers: Vec<Answer<'a>>,
        }
        Ok(serde_json::to_string(&Answers {
            answers: self
                .questions
                .iter()
                .zip(answers)
                .map(|(q, answer)| Answer { id: &q.id, answer })
                .collect(),
        })?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub id: String,
    pub request: Arc<Request>,
}
impl Pending {
    pub(crate) const ACCEPTED: &str = r#"{"accepted":true}"#;

    pub(crate) fn reply(&self, answers: Vec<String>) -> Result<String> {
        self.request.validate_answers(&answers)?;
        let mut text = format!("Answers to questionnaire {}:", self.id);
        for (q, answer) in self.request.questions().iter().zip(answers) {
            use std::fmt::Write;
            write!(text, "\n\n[{}] {}\n{}", q.id, q.question, answer).expect("write to string");
        }
        Ok(text)
    }
}
struct Waiting {
    pending: Pending,
    cancel: CancellationToken,
    reply: oneshot::Sender<String>,
}
/// One queue per main conversation; UI drafts are separately keyed by request ID.
pub struct Questionnaires {
    waiting: Mutex<Vec<Waiting>>,
    changed: Box<dyn Fn() + Send + Sync>,
}
impl Questionnaires {
    pub fn new(changed: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            waiting: Mutex::new(Vec::new()),
            changed: Box::new(changed),
        }
    }
    pub fn pending(&self) -> Vec<Pending> {
        self.waiting
            .lock()
            .expect("questionnaires")
            .iter()
            .filter(|w| !w.cancel.is_cancelled())
            .map(|w| w.pending.clone())
            .collect()
    }
    pub fn has_pending(&self) -> bool {
        self.waiting
            .lock()
            .expect("questionnaires")
            .iter()
            .any(|w| !w.cancel.is_cancelled())
    }
    pub async fn ask(&self, id: &str, args: &str, cancel: &CancellationToken) -> Result<String> {
        let request = Arc::new(Request::parse(args)?);
        let (reply, answer) = oneshot::channel();
        {
            let mut waiting = self.waiting.lock().expect("questionnaires");
            ensure!(!cancel.is_cancelled(), "Questionnaire cancelled");
            waiting.push(Waiting {
                pending: Pending {
                    id: id.into(),
                    request,
                },
                cancel: cancel.clone(),
                reply,
            });
        }
        (self.changed)();
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(anyhow::anyhow!("Questionnaire cancelled")),
            answer = answer => answer.context("Questionnaire closed"),
        };
        self.waiting
            .lock()
            .expect("questionnaires")
            .retain(|w| w.pending.id != id);
        (self.changed)();
        result
    }
    /// A failed validation leaves the request and all UI drafts available for retry.
    pub fn submit(&self, id: &str, answers: Vec<String>) -> Result<()> {
        let mut waiting = self.waiting.lock().expect("questionnaires");
        let index = waiting
            .iter()
            .position(|w| w.pending.id == id && !w.cancel.is_cancelled())
            .context("This questionnaire is no longer active")?;
        let answer = waiting[index].pending.request.answer(answers)?;
        if waiting.remove(index).reply.send(answer).is_err() {
            bail!("This questionnaire is no longer active");
        }
        drop(waiting);
        (self.changed)();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_boundaries_and_keeps_complete_ordered_answers() {
        let valid = json!({"questions":[
            {"id":"choice","question":"Platform?","kind":"select","options":["Linux", "Other"]},
            {"id":"notes","question":"Constraints?","kind":"text","options":false}
        ]});
        let request = Request::parse(&valid.to_string()).unwrap();
        let long = "λ界\n".repeat(30_000);
        let result: serde_json::Value = serde_json::from_str(
            &request
                .answer(vec!["Custom platform".into(), long.clone()])
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            result,
            json!({"answers":[{"id":"choice","answer":"Custom platform"},{"id":"notes","answer":long}]})
        );
        assert!(request.answer(vec!["Linux".into()]).is_err());
        assert!(
            request
                .answer(vec!["Linux".into(), "\u{2003}\n".into()])
                .is_err()
        );
        for invalid in [
            json!({"questions":[]}),
            json!({"questions":[{"id":"q","question":"?","kind":"multi"}]}),
            json!({"questions":[{"id":"q","question":"?","kind":"select","options":[]}]}),
            json!({"questions":[{"id":"q","question":"?","kind":"select","options":[" "]}]}),
            json!({"questions":[{"id":"q","question":"?","kind":"select","options":[false]}]}),
            json!({"questions":[{"id":"q","question":"?","kind":"select"}]}),
            json!({"questions":[{"id":" ","question":"?","kind":"text"}]}),
            json!({"questions":[{"id":"q","question":"\n","kind":"text"}]}),
            json!({"questions":[{"id":"q","question":"?","kind":"text","extra":true}]}),
        ] {
            assert!(Request::parse(&invalid.to_string()).is_err(), "{invalid}");
        }
        let mut duplicate = valid.clone();
        duplicate["questions"][1]["id"] = json!("choice");
        assert!(Request::parse(&duplicate.to_string()).is_err());
        let mut unicode_id = valid.clone();
        unicode_id["questions"][0]["id"] = json!("界".repeat(42));
        assert!(Request::parse(&unicode_id.to_string()).is_ok());
        unicode_id["questions"][0]["id"] = json!("界".repeat(43));
        assert!(Request::parse(&unicode_id.to_string()).is_err());
        let mut limits = json!({"questions":(0..24).map(|i| json!({"id":i.to_string(),"question":"?","kind":"select","options":vec!["x";20]})).collect::<Vec<_>>()});
        assert!(Request::parse(&limits.to_string()).is_ok());
        limits["questions"][0]["options"] = json!(vec!["x"; 21]);
        assert!(Request::parse(&limits.to_string()).is_err());
        let mut request = valid.to_string();
        request.extend(std::iter::repeat_n(' ', MAX_REQUEST_BYTES - request.len()));
        assert!(Request::parse(&request).is_ok());
        request.push(' ');
        assert!(Request::parse(&request).is_err());
    }

    #[tokio::test]
    async fn queue_routes_answers_and_rejects_stale_or_incomplete_submissions() {
        let (changed, mut changes) = tokio::sync::mpsc::unbounded_channel();
        let questions = Arc::new(Questionnaires::new(move || {
            let _ = changed.send(());
        }));
        let first_cancel = CancellationToken::new();
        let mut tasks = vec![];
        for (id, cancel) in [
            ("first", first_cancel.clone()),
            ("second", CancellationToken::new()),
        ] {
            let q = questions.clone();
            tasks.push(tokio::spawn(async move {
                q.ask(
                    id,
                    r#"{"questions":[{"id":"q","question":"Choose","kind":"text"}]}"#,
                    &cancel,
                )
                .await
            }));
            tokio::time::timeout(std::time::Duration::from_secs(2), changes.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(
            questions
                .pending()
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(questions.submit("first", vec![]).is_err());
        assert_eq!(questions.pending().len(), 2);
        questions
            .submit("second", vec!["Second answer".into()])
            .unwrap();
        assert!(
            questions
                .submit("second", vec!["Duplicate".into()])
                .is_err()
        );
        first_cancel.cancel();
        assert!(questions.submit("first", vec!["Too late".into()]).is_err());
        let second = tasks.pop().unwrap().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&second).unwrap(),
            json!({"answers":[{"id":"q","answer":"Second answer"}]})
        );
        assert!(tasks.pop().unwrap().await.unwrap().is_err());
        assert!(!questions.has_pending());
    }
}
