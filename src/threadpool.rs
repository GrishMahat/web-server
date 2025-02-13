use std::thread;
use std::sync::Arc;
use std::sync::mpsc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::fmt;

pub struct ThreadPool {
    workers: Vec<Worker>,
    sender: Option<mpsc::Sender<Message>>,
    active_count: Arc<AtomicUsize>,
}

#[allow(dead_code)]
struct Worker {
    id: usize,
    thread: Option<thread::JoinHandle<()>>,
}

enum Message {
    NewJob(Job),
    Terminate,
}

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug)]
pub enum ThreadPoolError {
    InvalidSize,
    JobSendError(String),
}

impl fmt::Display for ThreadPoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThreadPoolError::InvalidSize => write!(f, "Thread pool size must be greater than 0"),
            ThreadPoolError::JobSendError(msg) => write!(f, "Failed to send job: {}", msg),
        }
    }
}

impl std::error::Error for ThreadPoolError {}

impl ThreadPool {
    pub fn new(size: usize) -> Result<ThreadPool, ThreadPoolError> {
        if size == 0 {
            return Err(ThreadPoolError::InvalidSize);
        }

        let (sender, receiver) = mpsc::channel();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(size);
        let active_count = Arc::new(AtomicUsize::new(0));

        for id in 0..size {
            match Worker::new(id, Arc::clone(&receiver), Arc::clone(&active_count)) {
                Ok(worker) => workers.push(worker),
                Err(e) => {
                    for worker in &mut workers {
                        if let Some(thread) = worker.thread.take() {
                            let _ = thread.join();
                        }
                    }
                    return Err(ThreadPoolError::JobSendError(
                        format!("Failed to create worker {}: {}", id, e)
                    ));
                }
            }
        }

        Ok(ThreadPool {
            workers,
            sender: Some(sender),
            active_count,
        })
    }

    pub fn execute<F>(&self, f: F) -> Result<(), ThreadPoolError>
    where
        F: FnOnce() + Send + 'static,
    {
        let job = Box::new(f);
        if let Some(sender) = &self.sender {
            sender.send(Message::NewJob(job))
                .map_err(|e| ThreadPoolError::JobSendError(e.to_string()))?;
            Ok(())
        } else {
            Err(ThreadPoolError::JobSendError("Thread pool is shutting down".to_string()))
        }
    }

    pub fn active_count(&self) -> usize {
        self.active_count.load(Ordering::Relaxed)
    }

}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            for _ in &self.workers {
                let _ = sender.send(Message::Terminate);
            }
        }

        for worker in &mut self.workers {
            if let Some(thread) = worker.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

impl Worker {
    fn new(
        id: usize, 
        receiver: Arc<Mutex<mpsc::Receiver<Message>>>,
        active_count: Arc<AtomicUsize>,
    ) -> Result<Worker, String> {
        let thread = thread::Builder::new()
            .name(format!("worker-{}", id))
            .spawn(move || {
                loop {
                    let message = match receiver.lock() {
                        Ok(lock) => match lock.recv() {
                            Ok(msg) => msg,
                            Err(_) => break,
                        },
                        Err(_) => break,
                    };

                    match message {
                        Message::NewJob(job) => {
                            active_count.fetch_add(1, Ordering::Relaxed);
                            job();
                            active_count.fetch_sub(1, Ordering::Relaxed);
                        }
                        Message::Terminate => {
                            break;
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        Ok(Worker {
            id,
            thread: Some(thread),
        })
    }
}
