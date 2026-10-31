//! Business work is admitted and drained in the same session ownership domain.
use std::{collections::HashMap, future::Future, sync::Mutex};

use msgtrans::SessionId;
use tokio_util::task::TaskTracker;

#[derive(Default)]
pub(crate) struct SessionTasks {
    sessions: Mutex<HashMap<SessionId, TaskTracker>>,
}

impl SessionTasks {
    pub fn open(&self, id: SessionId) {
        self.sessions.lock().unwrap().entry(id).or_default();
    }

    pub fn spawn(&self, id: SessionId, work: impl Future<Output = ()> + Send + 'static) -> bool {
        let sessions = self.sessions.lock().unwrap();
        let Some(tasks) = sessions.get(&id) else {
            return false;
        };
        // Registration and retirement share this gate. No late task can be
        // inserted after drain takes ownership of the tracker.
        tasks.spawn(work);
        true
    }

    pub async fn drain(&self, id: SessionId) {
        let tasks = self.sessions.lock().unwrap().remove(&id);
        if let Some(tasks) = tasks {
            tasks.close();
            // Do not cancel a partially committed authentication operation.
            // Its effects finish before the caller removes identity bindings.
            tasks.wait().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    #[tokio::test]
    async fn retirement_waits_for_admitted_work_and_refuses_late_work() {
        let tasks = Arc::new(SessionTasks::default());
        let id = SessionId::new(1);
        tasks.open(id);
        let committed = Arc::new(AtomicBool::new(false));
        let flag = committed.clone();
        let (release, wait) = tokio::sync::oneshot::channel();
        assert!(tasks.spawn(id, async move {
            wait.await.unwrap();
            flag.store(true, Ordering::SeqCst);
        }));
        let mut draining = Box::pin(tasks.drain(id));
        assert!(futures::poll!(&mut draining).is_pending());
        assert!(!tasks.spawn(id, async { panic!("retired session admitted work") }));
        release.send(()).unwrap();
        draining.await;
        assert!(committed.load(Ordering::SeqCst));
        tasks.drain(id).await;
    }
}
