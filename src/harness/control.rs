use super::model::{Event, Mode, id};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, mpsc};
use tokio::sync::{oneshot, watch};

struct State {
    paused: bool,
    stopped: bool,
    mode: Mode,
    notes: VecDeque<(String, String)>,
    answers: HashMap<String, oneshot::Sender<String>>,
}

pub struct Control {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    persistence: Mutex<Option<(super::store::Store, String, mpsc::Sender<Event>)>>,
    output: Mutex<String>,
    jobs: Mutex<HashMap<String, std::sync::Arc<Control>>>,
}

impl Control {
    pub fn new(mode: Mode) -> Self {
        Self {
            state: Mutex::new(State {
                paused: false,
                stopped: false,
                mode,
                notes: VecDeque::new(),
                answers: HashMap::new(),
            }),
            changed: watch::channel(0).0,
            persistence: Mutex::new(None),
            output: Mutex::new(String::new()),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    fn wake(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }
    pub fn stopped(&self) -> bool {
        self.state.lock().unwrap().stopped
    }
    pub fn paused(&self) -> bool {
        self.state.lock().unwrap().paused
    }
    pub fn mode(&self) -> Mode {
        self.state.lock().unwrap().mode
    }

    pub fn attach(
        &self,
        store: super::store::Store,
        run: String,
        events: mpsc::Sender<Event>,
    ) -> Result<(), String> {
        let pending = store
            .load(&format!("runs/{run}/pending.json"))?
            .unwrap_or_default();
        self.state.lock().unwrap().notes = pending;
        *self.persistence.lock().unwrap() = Some((store, run, events));
        Ok(())
    }

    fn persist_notes(&self, notes: &VecDeque<(String, String)>) {
        if let Some((store, run, events)) = self.persistence.lock().unwrap().as_ref()
            && let Err(error) = store.save(&format!("runs/{run}/pending.json"), notes)
        {
            let _ = events.send(Event::Trace(super::model::Trace {
                run: run.clone(),
                time: super::model::now(),
                kind: "storage_error".into(),
                detail: serde_json::json!(error),
            }));
        }
    }

    pub fn append_output(&self, text: &str) {
        let mut output = self.output.lock().unwrap();
        if output.len() < 1_024_000 {
            output.push_str(text);
        }
    }
    pub fn output(&self) -> String {
        self.output.lock().unwrap().clone()
    }

    pub fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.stopped = true;
        state.answers.clear();
        drop(state);
        for job in self.jobs.lock().unwrap().values() {
            job.stop();
        }
        self.wake();
    }

    pub fn register_job(&self, id: String, control: std::sync::Arc<Control>) {
        self.jobs.lock().unwrap().insert(id, control);
    }
    pub fn stop_job(&self, id: &str) -> bool {
        if let Some(job) = self.jobs.lock().unwrap().get(id) {
            job.stop();
            true
        } else {
            false
        }
    }
    pub fn job_output(&self, id: &str) -> Option<String> {
        self.jobs.lock().unwrap().get(id).map(|c| c.output())
    }

    pub fn pause(&self, paused: bool) {
        self.state.lock().unwrap().paused = paused;
        self.wake();
    }

    pub fn set_mode(&self, mode: Mode) {
        self.state.lock().unwrap().mode = mode;
        self.steer(
            "mode",
            format!("The user switched to {mode}. Tool policy is now enforced in this mode."),
        );
    }

    pub fn steer(&self, source: &str, text: String) {
        let mut state = self.state.lock().unwrap();
        state.notes.push_back((source.into(), text.clone()));
        self.persist_notes(&state.notes);
        drop(state);
        if let Some((store, run, events)) = self.persistence.lock().unwrap().as_ref() {
            let trace = super::model::Trace {
                run: run.clone(),
                time: super::model::now(),
                kind: "steering_enqueued".into(),
                detail: serde_json::json!({"source":source,"text":text}),
            };
            if let Err(error) = store.trace(&trace) {
                let _ = events.send(Event::Trace(super::model::Trace {
                    run: run.clone(),
                    time: super::model::now(),
                    kind: "storage_error".into(),
                    detail: serde_json::json!(error),
                }));
            }
            let _ = events.send(Event::Trace(trace));
        }
        self.wake();
    }

    pub fn drain(&self) -> Vec<(String, String)> {
        let mut state = self.state.lock().unwrap();
        let notes = state.notes.drain(..).collect();
        self.persist_notes(&state.notes);
        notes
    }

    pub fn answer(&self, question: &str, answer: String) {
        if let Some(sender) = self.state.lock().unwrap().answers.remove(question) {
            let _ = sender.send(answer);
        }
        self.wake();
    }

    pub async fn checkpoint(&self) -> Result<(), String> {
        let mut changed = self.changed.subscribe();
        loop {
            {
                let state = self.state.lock().unwrap();
                if state.stopped {
                    return Err("Stopped by user".into());
                }
                if !state.paused {
                    return Ok(());
                }
            }
            changed.changed().await.map_err(|e| e.to_string())?;
        }
    }

    pub async fn cancelled(&self) {
        let mut changed = self.changed.subscribe();
        while !self.stopped() {
            if changed.changed().await.is_err() {
                break;
            }
        }
    }

    pub async fn ask(
        &self,
        run: &str,
        prompt: String,
        approval: bool,
        events: &mpsc::Sender<Event>,
    ) -> Result<String, String> {
        self.checkpoint().await?;
        let question = id();
        let (sender, receiver) = oneshot::channel();
        self.state
            .lock()
            .unwrap()
            .answers
            .insert(question.clone(), sender);
        events
            .send(Event::Question {
                run: run.into(),
                id: question.clone(),
                prompt,
                approval,
            })
            .map_err(|e| e.to_string())?;
        let answer = tokio::select! {
            result = receiver => result.map_err(|_| "Question cancelled".to_string()),
            _ = self.cancelled() => Err("Stopped by user".into()),
        };
        self.state.lock().unwrap().answers.remove(&question);
        let _ = events.send(Event::Answered {
            run: run.into(),
            id: question,
        });
        self.checkpoint().await?;
        answer
    }
}
