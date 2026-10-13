use super::*;

struct Resource {
    dropped: mpsc::Sender<std::thread::ThreadId>,
    release_drop: Option<mpsc::Receiver<()>>,
}
impl Drop for Resource {
    fn drop(&mut self) {
        self.dropped.send(std::thread::current().id()).unwrap();
        if let Some(release) = self.release_drop.take() {
            release.recv().unwrap();
        }
    }
}

fn wait_released(capacity: &Capacity) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while capacity.0.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[tokio::test]
async fn stalled_initializer_does_not_block_runtime_or_allow_more_workers() {
    let capacity = Capacity::default();
    let (resume, blocked) = mpsc::channel();
    let (dropped, destruction) = mpsc::channel();
    let (release_drop, blocked_drop) = mpsc::channel();
    let mut init = Initializer::start_in(capacity.clone(), Duration::from_millis(1), move || {
        blocked.recv().unwrap();
        Ok(Resource {
            dropped,
            release_drop: Some(blocked_drop),
        })
    })
    .unwrap();
    // A current-thread runtime remains able to run its own timers/controls.
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert!(matches!(init.poll(Instant::now()), Poll::TimedOut));
    drop(init);
    for _ in 0..20 {
        assert!(
            Initializer::start_in(capacity.clone(), Duration::from_secs(1), || Ok(())).is_err()
        );
    }
    resume.send(()).unwrap();
    let thread = destruction.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_ne!(thread, std::thread::current().id());
    // A blocked destructor also owns capacity; timeout never frees native state.
    assert!(Initializer::start_in(capacity.clone(), Duration::from_secs(1), || Ok(())).is_err());
    release_drop.send(()).unwrap();
    wait_released(&capacity);
}

#[test]
fn cancelling_an_already_completed_result_still_destroys_it_on_worker() {
    let capacity = Capacity::default();
    let (dropped, destruction) = mpsc::channel();
    let init = Initializer::start_in(capacity.clone(), Duration::from_secs(1), move || {
        Ok(Resource {
            dropped,
            release_drop: None,
        })
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while init.result.lock().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    drop(init);
    assert_ne!(
        destruction.recv_timeout(Duration::from_secs(2)).unwrap(),
        std::thread::current().id()
    );
    wait_released(&capacity);
}

#[test]
fn result_completed_after_deadline_is_retired_without_adoption() {
    let capacity = Capacity::default();
    let (resume, blocked) = mpsc::channel();
    let (dropped, destruction) = mpsc::channel();
    let mut init = Initializer::start_in(capacity.clone(), Duration::from_secs(2), move || {
        blocked.recv().unwrap();
        Ok(Resource {
            dropped,
            release_drop: None,
        })
    })
    .unwrap();
    init.deadline = Instant::now();
    resume.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while init.result.lock().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(matches!(init.poll(Instant::now()), Poll::TimedOut));
    assert!(destruction.try_recv().is_err());
    drop(init);
    assert_ne!(
        destruction.recv_timeout(Duration::from_secs(2)).unwrap(),
        std::thread::current().id()
    );
    wait_released(&capacity);
}

#[test]
fn successful_adoption_transfers_exactly_once_and_releases_startup_capacity() {
    let capacity = Capacity::default();
    let (dropped, destruction) = mpsc::channel();
    let mut init = Initializer::start_in(capacity.clone(), Duration::from_secs(2), move || {
        Ok(Resource {
            dropped,
            release_drop: None,
        })
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let resource = loop {
        match init.poll(Instant::now()) {
            Poll::Ready(result) => break result.unwrap(),
            Poll::Pending => {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            Poll::TimedOut => panic!("worker did not finish"),
        }
    };
    drop(init);
    wait_released(&capacity);
    assert!(destruction.try_recv().is_err());
    drop(resource);
    assert_eq!(destruction.recv().unwrap(), std::thread::current().id());
    assert!(destruction.try_recv().is_err());
}
