//! VRAM drawn on a second thread.
//!
//! The GPU's drawing time never depends on what VRAM holds (only on the
//! primitives' geometry and the texture cache tags), so a command's pixels
//! can be drawn later, by another thread, as long as nothing looks at VRAM
//! first. [`Painter`] queues each command's pixel work as a [`Job`] and a
//! worker thread runs them in order; [`Painter::lock`] (GPUREAD, a dump, a
//! save state) and [`Painter::lock_mut`] wait for every job queued so far.
//! The worker is then idle until the next job, which needs `&mut Painter`,
//! so read guards can be held together.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::raster::{self, Raster, Shade, Vtx};
use crate::{PageTexture, Vram, VramTexture};

/// One command's pixel work.
#[derive(Clone, Copy)]
pub(crate) enum Job {
    Tri {
        vs: [Vtx; 3],
        r: Raster,
        sh: Shade,
        tex: VramTexture,
    },
    Line {
        ab: [Vtx; 2],
        r: Raster,
        sh: Shade,
    },
    Rect {
        o: Vtx,
        w: i32,
        h: i32,
        flip: (bool, bool),
        r: Raster,
        sh: Shade,
        tex: VramTexture,
    },
    Fill {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        value: u16,
        skip: Option<i32>,
    },
    Copy {
        s: (i32, i32),
        d: (i32, i32),
        w: i32,
        h: i32,
        check: bool,
        set: u16,
    },
    /// Up to two pixels of a CPU→VRAM transfer.
    Pixels {
        at: [(i32, i32); 2],
        n: u8,
        data: u32,
        check: bool,
        set: u16,
    },
}

impl Job {
    fn run(self, vram: &mut Vram) {
        match self {
            Job::Tri { vs, r, sh, tex } => {
                if !sh.textured {
                    raster::triangle(vram, &raster::NoTexture, &r, &sh, vs);
                } else {
                    with_page_texture!(tex, vram, r.clip, |t| raster::triangle(vram, t, &r, &sh, vs));
                }
            }
            Job::Line { ab, r, sh } => raster::line(vram, &r, &sh, ab[0], ab[1]),
            Job::Rect { o, w, h, flip, r, sh, tex } => {
                if !sh.textured {
                    raster::rect(vram, &raster::NoTexture, &r, &sh, o, w, h, flip);
                } else {
                    with_page_texture!(tex, vram, r.clip, |t| raster::rect(vram, t, &r, &sh, o, w, h, flip));
                }
            }
            Job::Fill { x, y, w, h, value, skip } => {
                for row in (0..h).filter(|row| skip != Some((y + row) & 1)) {
                    for col in 0..w {
                        vram.set(x + col, y + row, value);
                    }
                }
            }
            Job::Copy { s, d, w, h, check, set } => {
                let mut line = Vec::with_capacity(w as usize);
                for row in 0..h {
                    line.clear();
                    line.extend((0..w).map(|c| vram.get(s.0 + c, s.1 + row)));
                    for (c, &v) in line.iter().enumerate() {
                        let (x, y) = (d.0 + c as i32, d.1 + row);
                        if check && vram.get(x, y) & 0x8000 != 0 {
                            continue;
                        }
                        vram.set(x, y, v | set);
                    }
                }
            }
            Job::Pixels { at, n, mut data, check, set } => {
                for &(x, y) in &at[..n as usize] {
                    if !(check && vram.get(x, y) & 0x8000 != 0) {
                        vram.set(x, y, data as u16 | set);
                    }
                    data >>= 16;
                }
            }
        }
    }
}

/// Jobs handed to the worker at once.
const BATCH: usize = 256;

struct Worker {
    tx: Sender<Vec<Job>>,
    /// Batches back, emptied, once drawn.
    done: Receiver<Vec<Job>>,
    out: usize,
    spare: Vec<Vec<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    fn spawn(vram: Arc<RwLock<Vram>>) -> Option<Worker> {
        let (tx, rx) = channel::<Vec<Job>>();
        let (done_tx, done) = channel();
        let thread = std::thread::Builder::new()
            .name("psx-gpu-paint".into())
            .spawn(move || {
                for mut batch in rx {
                    let mut vram = vram.write().unwrap_or_else(|e| e.into_inner());
                    for job in batch.drain(..) {
                        job.run(&mut vram);
                    }
                    drop(vram);
                    if done_tx.send(batch).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Worker { tx, done, out: 0, spare: Vec::new(), thread: Some(thread) })
    }

    fn wait(&mut self) {
        while self.out > 0 {
            let batch = self.done.recv().expect("psx-gpu paint thread");
            self.spare.push(batch);
            self.out -= 1;
        }
    }
}

struct Queue {
    batch: Vec<Job>,
    /// `None`: drawing inline (one CPU, or the thread could not start).
    worker: Option<Worker>,
    started: bool,
}

/// VRAM whose drawing runs on a worker thread; see the module comment.
pub struct Painter {
    vram: Arc<RwLock<Vram>>,
    queue: Mutex<Queue>,
    /// Jobs queued or drawing: a read must wait.
    busy: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn threads_allowed() -> bool {
    std::env::var_os("PSX_GPU_THREAD").is_none_or(|v| v != "0")
        && std::thread::available_parallelism().is_ok_and(|n| n.get() > 1)
}

impl Painter {
    pub fn new(vram: Vram) -> Self {
        Painter {
            vram: Arc::new(RwLock::new(vram)),
            queue: Mutex::new(Queue { batch: Vec::with_capacity(BATCH), worker: None, started: false }),
            busy: AtomicBool::new(false),
        }
    }

    /// Queue a command's pixels (drawn at once without a worker).
    pub(crate) fn push(&mut self, job: Job) {
        let q = self.queue.get_mut().unwrap_or_else(|e| e.into_inner());
        if !q.started {
            q.started = true;
            q.worker = threads_allowed().then(|| Worker::spawn(self.vram.clone())).flatten();
        }
        let Some(w) = &mut q.worker else {
            job.run(&mut self.vram.write().unwrap_or_else(|e| e.into_inner()));
            return;
        };
        q.batch.push(job);
        *self.busy.get_mut() = true;
        if q.batch.len() >= BATCH {
            let next = w.spare.pop().unwrap_or_else(|| Vec::with_capacity(BATCH));
            let full = std::mem::replace(&mut q.batch, next);
            w.tx.send(full).expect("psx-gpu paint thread");
            w.out += 1;
        }
    }

    /// Wait for every queued job.
    fn sync(&self) {
        if !self.busy.load(Ordering::Acquire) {
            return;
        }
        let mut guard = lock(&self.queue);
        let q = &mut *guard;
        if let Some(w) = &mut q.worker {
            if !q.batch.is_empty() {
                let next = w.spare.pop().unwrap_or_else(|| Vec::with_capacity(BATCH));
                w.tx.send(std::mem::replace(&mut q.batch, next)).expect("psx-gpu paint thread");
                w.out += 1;
            }
            // The worker is idle once everything sent is back.
            w.wait();
        }
        self.busy.store(false, Ordering::Release);
    }
}

impl Painter {
    /// The VRAM, once every job queued so far is drawn.
    pub fn lock(&self) -> RwLockReadGuard<'_, Vram> {
        self.sync();
        self.vram.read().unwrap_or_else(|e| e.into_inner())
    }

    /// As [`Painter::lock`], to write.
    pub fn lock_mut(&mut self) -> RwLockWriteGuard<'_, Vram> {
        self.sync();
        self.vram.write().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        let q = self.queue.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(mut w) = q.worker.take() {
            drop(w.tx);
            if let Some(t) = w.thread.take() {
                let _ = t.join();
            }
        }
    }
}

impl Default for Painter {
    fn default() -> Self {
        Painter::new(Vram::default())
    }
}

impl Clone for Painter {
    fn clone(&self) -> Self {
        Painter::new(self.lock().clone())
    }
}

impl std::fmt::Debug for Painter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.lock().fmt(f)
    }
}

impl serde::Serialize for Painter {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.lock().serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for Painter {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vram::deserialize(d).map(Painter::new)
    }
}

impl From<Vram> for Painter {
    fn from(v: Vram) -> Self {
        Painter::new(v)
    }
}
