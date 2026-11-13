//! One line per thing the app is doing.
//!
//! The pane shows these in order. A step is never folded into a neighbor
//! ("thinking or checking cookies" hid which of those was actually stuck).

use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityStep {
    CheckingSavedSession,
    UsingEnvironmentSession,
    ClearingSavedSession,
    OpeningBrowser(String),
    WaitingForSignIn(String),
    ReadingSessionCookies,
    SavingSession,
    OpeningGemini,
    LoadingModels,
    CheckingQuota,
    SendingPrompt,
    ReadingReply,
    RetryingWithoutModel,
}

impl ActivityStep {
    pub fn label(&self) -> String {
        match self {
            Self::CheckingSavedSession => "Checking the saved session".into(),
            Self::UsingEnvironmentSession => "Using GEMINI_PSID from the environment".into(),
            Self::ClearingSavedSession => "Clearing the saved session".into(),
            Self::OpeningBrowser(name) => format!("Opening {name}"),
            Self::WaitingForSignIn(name) => format!("Waiting for you to sign in with {name}"),
            Self::ReadingSessionCookies => "Reading the session cookies".into(),
            Self::SavingSession => "Saving the session".into(),
            Self::OpeningGemini => "Opening Gemini".into(),
            Self::LoadingModels => "Loading models".into(),
            Self::CheckingQuota => "Checking quota".into(),
            Self::SendingPrompt => "Sending the prompt".into(),
            Self::ReadingReply => "Reading the reply".into(),
            Self::RetryingWithoutModel => "Retrying without the model header".into(),
        }
    }
}

impl std::fmt::Display for ActivityStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

/// Keeps the first time a step is announced and drops an immediate repeat.
pub fn record_step(steps: &mut Vec<ActivityStep>, step: ActivityStep) {
    if steps.last() != Some(&step) {
        steps.push(step);
    }
}

/// Sends a step to the pane. `GEMINI_TRACE=1` also prints it, which is the
/// easiest way to see where a request stopped when the window is frozen.
#[derive(Clone)]
pub struct Reporter {
    inner: Arc<dyn Fn(ActivityStep) + Send + Sync>,
}

impl Reporter {
    pub fn new(report: impl Fn(ActivityStep) + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(report),
        }
    }

    pub fn noop() -> Self {
        Self::new(|_| {})
    }

    pub fn report(&self, step: ActivityStep) {
        if std::env::var_os("GEMINI_TRACE").is_some() {
            eprintln!("gemini: {step}");
        }
        (self.inner)(step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_step_has_its_own_label() {
        let steps = [
            ActivityStep::CheckingSavedSession,
            ActivityStep::UsingEnvironmentSession,
            ActivityStep::ClearingSavedSession,
            ActivityStep::OpeningBrowser("Chrome".into()),
            ActivityStep::WaitingForSignIn("Chrome".into()),
            ActivityStep::ReadingSessionCookies,
            ActivityStep::SavingSession,
            ActivityStep::OpeningGemini,
            ActivityStep::LoadingModels,
            ActivityStep::CheckingQuota,
            ActivityStep::SendingPrompt,
            ActivityStep::ReadingReply,
            ActivityStep::RetryingWithoutModel,
        ];
        let labels: Vec<String> = steps.iter().map(ActivityStep::label).collect();
        for label in &labels {
            let folded = label.to_lowercase();
            assert!(!folded.contains("thinking"), "{label}");
            assert!(!label.contains(" or "), "{label}");
            assert!(!label.is_empty());
        }
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), labels.len());
    }

    #[test]
    fn immediate_repeat_is_one_row() {
        let mut steps = Vec::new();
        record_step(&mut steps, ActivityStep::SendingPrompt);
        record_step(&mut steps, ActivityStep::SendingPrompt);
        record_step(&mut steps, ActivityStep::ReadingReply);
        record_step(&mut steps, ActivityStep::SendingPrompt);
        assert_eq!(
            steps,
            vec![
                ActivityStep::SendingPrompt,
                ActivityStep::ReadingReply,
                ActivityStep::SendingPrompt,
            ]
        );
    }

    #[test]
    fn reporter_forwards_every_step() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let report = Reporter::new({
            let seen = Arc::clone(&seen);
            move |step| seen.lock().expect("steps").push(step)
        });
        report.report(ActivityStep::OpeningGemini);
        report.report(ActivityStep::LoadingModels);
        let steps = seen.lock().expect("steps");
        assert_eq!(
            steps.as_slice(),
            &[ActivityStep::OpeningGemini, ActivityStep::LoadingModels]
        );
    }
}
