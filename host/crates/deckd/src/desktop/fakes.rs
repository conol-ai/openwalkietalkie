//! The stand-in every test raises apps with: no test may bring an app
//! forward on the machine it runs on. Not `cfg(test)`, so the crate's
//! integration tests can build a desktop over it too.
use super::raise::Raiser;
use std::sync::Mutex;

/// A [`Raiser`] over a list of "running" bundle identifiers, which records
/// every app it was asked to raise and can be told to refuse.
#[derive(Debug, Default)]
pub struct FakeRaiser {
    running: Vec<String>,
    refusal: Option<String>,
    raised: Mutex<Vec<String>>,
}

impl FakeRaiser {
    #[must_use]
    pub fn new() -> FakeRaiser {
        FakeRaiser::default()
    }

    /// `app` counts as running.
    #[must_use]
    pub fn with_running(mut self, app: &str) -> FakeRaiser {
        self.running.push(app.to_string());
        self
    }

    /// Every raise is refused with `why`.
    #[must_use]
    pub fn refusing(mut self, why: &str) -> FakeRaiser {
        self.refusal = Some(why.to_string());
        self
    }

    /// The apps `raise` was asked for, in order.
    #[must_use]
    pub fn raised(&self) -> Vec<String> {
        self.raised.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

impl Raiser for FakeRaiser {
    fn running(&self, app: &str) -> bool {
        self.running.iter().any(|running| running == app)
    }

    fn raise(&self, app: &str) -> Result<(), String> {
        if let Ok(mut raised) = self.raised.lock() {
            raised.push(app.to_string());
        }
        match &self.refusal {
            Some(why) => Err(why.clone()),
            None => Ok(()),
        }
    }

    fn name(&self) -> &'static str {
        "fake"
    }
}
