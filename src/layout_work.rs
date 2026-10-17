//! One running layout and one replaceable pending layout. Cancellation is
//! cooperative; cancellation acknowledgement joins the actual worker.
use crate::projection::TuiSessionProjection;
use ratatui::layout::Rect;
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

thread_local! { static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) }; }
#[cfg(test)]
thread_local! { static PROGRESS: RefCell<Option<(Arc<std::sync::atomic::AtomicUsize>, Arc<tokio::sync::Notify>)>> = const { RefCell::new(None) }; }
pub(crate) fn cancelled() -> bool {
    #[cfg(test)]
    PROGRESS.with(|slot| {
        if let Some((count, notify)) = slot.borrow().as_ref()
            && count.fetch_add(1, Ordering::Relaxed) == 0
        {
            notify.notify_one();
        }
    });
    CANCEL.with(|token| {
        token
            .borrow()
            .as_ref()
            .is_some_and(|token| token.load(Ordering::Relaxed))
    })
}

#[derive(Clone, PartialEq, Eq)]
struct Key {
    session: String,
    generation: u64,
    viewport: Rect,
}
struct Job {
    key: Key,
    projection: TuiSessionProjection,
}
struct Active {
    key: Key,
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Option<TuiSessionProjection>, &'static str>>,
    worker: thread::JoinHandle<()>,
    #[cfg(test)]
    progress: Arc<std::sync::atomic::AtomicUsize>,
}
pub(crate) struct LayoutWork {
    pub(crate) enabled: bool,
    pub(crate) notify: Arc<tokio::sync::Notify>,
    pub(crate) error: Option<&'static str>,
    desired: Option<Key>,
    active: Option<Active>,
    pending: Option<Job>,
    #[cfg(test)]
    started: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    fail_next: bool,
}
impl Default for LayoutWork {
    fn default() -> Self {
        Self {
            enabled: !cfg!(test),
            notify: Arc::new(tokio::sync::Notify::new()),
            error: None,
            desired: None,
            active: None,
            pending: None,
            #[cfg(test)]
            started: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            fail_next: false,
        }
    }
}
impl LayoutWork {
    pub(crate) fn update(
        &mut self,
        projection: &TuiSessionProjection,
        viewport: Rect,
    ) -> Option<TuiSessionProjection> {
        let key = Key {
            session: projection.session_id.clone(),
            generation: projection.output_generation,
            viewport,
        };
        if self.desired.as_ref() != Some(&key) {
            self.error = None;
            if let Some(active) = &self.active {
                active.cancel.store(true, Ordering::Relaxed);
            }
            self.desired = Some(key.clone());
            self.pending = Some(Job {
                key,
                projection: projection.layout_snapshot(),
            });
        }
        let completed = self
            .active
            .as_ref()
            .and_then(|active| match active.result.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err("layout worker disconnected")),
                Err(mpsc::TryRecvError::Empty) => None,
            });
        let mut install = None;
        if let Some(result) = completed {
            let active = self.active.take().unwrap();
            let joined = active.worker.join().is_ok();
            if joined
                && self.desired.as_ref() == Some(&active.key)
                && !active.cancel.load(Ordering::Relaxed)
            {
                match result {
                    Ok(layout) => install = layout,
                    Err(error) => self.error = Some(error),
                }
            }
        }
        if self.active.is_none()
            && let Some(job) = self.pending.take()
        {
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let notify = self.notify.clone();
            let (sender, result) = mpsc::sync_channel(1);
            let viewport = job.key.viewport;
            #[cfg(test)]
            let progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            #[cfg(test)]
            let worker_progress = (progress.clone(), self.started.clone());
            #[cfg(test)]
            let fail_next = std::mem::take(&mut self.fail_next);
            let worker = thread::spawn(move || {
                #[cfg(test)]
                PROGRESS.with(|slot| *slot.borrow_mut() = Some(worker_progress));
                CANCEL.with(|slot| *slot.borrow_mut() = Some(worker_cancel.clone()));
                let computed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    #[cfg(test)]
                    if fail_next {
                        panic!("injected layout worker failure");
                    }
                    let mut projection = job.projection;
                    crate::ui::build_background_layout(&mut projection, viewport);
                    projection
                }))
                .map(|projection| (!worker_cancel.load(Ordering::Relaxed)).then_some(projection))
                .map_err(|_| "layout worker panicked");
                CANCEL.with(|slot| *slot.borrow_mut() = None);
                #[cfg(test)]
                PROGRESS.with(|slot| *slot.borrow_mut() = None);
                let _ = sender.send(computed);
                notify.notify_one();
            });
            self.active = Some(Active {
                key: job.key,
                cancel,
                result,
                worker,
                #[cfg(test)]
                progress,
            });
        }
        install
    }
    pub(crate) fn cancel_and_wait(&mut self) {
        self.pending = None;
        self.desired = None;
        self.error = None;
        if let Some(active) = self.active.take() {
            active.cancel.store(true, Ordering::Relaxed);
            let _ = active.worker.join();
        }
    }
}
impl Drop for LayoutWork {
    fn drop(&mut self) {
        self.cancel_and_wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        commands::AgentMode,
        model::{DisplayContent, DisplayEntry, DisplayKind},
    };
    #[tokio::test]
    async fn worker_failure_is_observable_and_new_generation_recovers() {
        let mut projection = TuiSessionProjection::new("a".into(), AgentMode::Build, None);
        projection.push_entry(DisplayEntry {
            kind: DisplayKind::User,
            content: DisplayContent::Markdown("recover".into()),
        });
        let mut work = LayoutWork::default();
        work.fail_next = true;
        let viewport = Rect::new(0, 0, 80, 20);
        assert!(work.update(&projection, viewport).is_none());
        work.notify.notified().await;
        assert!(work.update(&projection, viewport).is_none());
        assert_eq!(work.error, Some("layout worker panicked"));
        assert!(work.active.is_none() && work.pending.is_none());
        projection.invalidate_output_layout();
        assert!(work.update(&projection, viewport).is_none());
        work.notify.notified().await;
        assert!(
            work.update(&projection, viewport)
                .unwrap()
                .message_layout
                .unwrap()
                .text
                .contains("recover")
        );
        assert!(work.error.is_none());
    }
    #[tokio::test]
    async fn cancellation_joins_started_cpu_work_and_keeps_only_latest_pending() {
        let mut projection = TuiSessionProjection::new("a".into(), AgentMode::Build, None);
        projection.push_entry(DisplayEntry {
            kind: DisplayKind::Assistant,
            content: DisplayContent::Markdown("正文 code 中文 \n".repeat(80_000)),
        });
        let mut work = LayoutWork::default();
        work.update(&projection, Rect::new(0, 0, 80, 20));
        tokio::time::timeout(std::time::Duration::from_secs(10), work.started.notified())
            .await
            .unwrap();
        let progress = work.active.as_ref().unwrap().progress.clone();
        assert!(progress.load(Ordering::Relaxed) > 0);
        // Replacements never start a second worker until the first is joined.
        for width in 40..70 {
            work.update(&projection, Rect::new(0, 0, width, 20));
        }
        assert_eq!(work.desired.as_ref().unwrap().viewport.width, 69);
        work.cancel_and_wait();
        assert!(work.active.is_none() && work.pending.is_none());
        let stopped_count = progress.load(Ordering::Relaxed);
        assert!(stopped_count > 0); // Join is the CPU stop acknowledgement.
        assert_eq!(progress.load(Ordering::Relaxed), stopped_count);
    }
}
