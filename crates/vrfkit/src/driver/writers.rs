//! Parquet writers running off the packet loop.
//!
//! `fields` and `movement` are the two large tables and their Parquet encoding
//! (Arrow batch build + ZSTD) was measured at 570 ms and 450 ms of a 2.60 s
//! export -- 37% of the run, executed inline in the packet loop. Each table is
//! an independent file whose writer never reads replay state, so each is moved
//! to its own thread and fed record batches over a bounded channel. The writers
//! still see every record exactly once, in stream order, and the row-group flush
//! boundary still falls on the same cumulative row counts, so the bytes are
//! unchanged; only the thread they are produced on differs.
//!
//! The channels are bounded so a slow writer applies backpressure instead of
//! growing the in-flight batch queue without limit. `actors`, `net_guids` and
//! `events` stay inline: together they are under 1% of the write cost.
//!
//! No error is dropped on this path. A writer that fails returns its error and
//! drops its receiver, which turns the next `send` into an error. `ship` then
//! joins the thread on the spot and returns the writer's own error -- the one
//! the packet loop propagates, since the driver returns from there without
//! reaching `finish`, and `Drop` has to discard whatever it joins. A writer
//! thread that panics is reported as an error rather than being mistaken for
//! success, and a writer that has failed can never `finish` successfully.

use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;

use vrf_export::ExportError;

use crate::error::CliError;

/// Rows accumulated in the packet loop before a batch is handed to a writer
/// thread. A replay yields ~530 k packets but only ~0.8 field rows and ~3.5
/// movement rows per packet, so sending one message per packet would cost more
/// in channel traffic than the encoding it hides. At this size `fields` sends
/// ~26 messages and `movement` ~112 over a whole replay.
const WRITER_BATCH_ROWS: usize = 16_384;

/// Batches allowed in flight per writer. Bounds peak memory: four field batches
/// is roughly 10 MB of records plus the raw-bit payloads they own. The two
/// name columns no longer contribute -- they are interned `Arc<str>` shared
/// with the sink, so a queued batch holds refcounts, not strings.
const WRITER_QUEUE_DEPTH: usize = 4;

/// A writer running on its own thread, plus the handle needed to collect its
/// result. `T` is the record type of the table it owns.
pub(super) struct WriterThread<T> {
    tx: Option<SyncSender<Vec<T>>>,
    handle: Option<thread::JoinHandle<Result<(), ExportError>>>,
    batch: Vec<T>,
    /// Table name, used only to name the failing table in an error message.
    table: &'static str,
    /// Why the writer stopped, once `ship` has found out and joined it. The
    /// error itself went to that caller (it is not `Clone`); the text is kept
    /// so a later `finish` names the same cause instead of claiming success.
    failure: Option<String>,
}

impl<T: Send + 'static> WriterThread<T> {
    /// Spawn a writer thread driven by `run`, which consumes every batch in
    /// stream order and then finalises the file.
    pub(super) fn spawn<F>(table: &'static str, run: F) -> Self
    where
        F: FnOnce(std::sync::mpsc::Receiver<Vec<T>>) -> Result<(), ExportError> + Send + 'static,
    {
        let (tx, rx) = sync_channel::<Vec<T>>(WRITER_QUEUE_DEPTH);
        let handle = thread::spawn(move || run(rx));
        Self {
            tx: Some(tx),
            handle: Some(handle),
            batch: Vec::with_capacity(WRITER_BATCH_ROWS),
            table,
            failure: None,
        }
    }

    /// Move `records` into the pending batch, shipping it once it is full.
    pub(super) fn append(&mut self, records: &mut Vec<T>) -> Result<(), CliError> {
        self.batch.append(records);
        if self.batch.len() >= WRITER_BATCH_ROWS {
            self.ship()?;
        }
        Ok(())
    }

    fn ship(&mut self) -> Result<(), CliError> {
        let full = std::mem::replace(&mut self.batch, Vec::with_capacity(WRITER_BATCH_ROWS));
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| CliError::Usage(format!("{} writer already closed", self.table)))?;
        if tx.send(full).is_ok() {
            return Ok(());
        }
        // A send fails only once the receiver is gone, so the writer thread
        // has already returned or panicked and this join cannot block. It is
        // the last place its own error exists: the packet loop returns what
        // this returns without ever calling `finish`, and `Drop` discards the
        // result it joins. "Stopped early" used to be the whole message even
        // for a disk-full or Parquet encode failure.
        self.tx = None;
        let error = match self.handle.take().map(thread::JoinHandle::join) {
            Some(Ok(Err(e))) => CliError::Export(e),
            Some(Err(_)) => CliError::Usage(format!("{} writer thread panicked", self.table)),
            // Returned `Ok` without draining its channel: nothing more
            // specific to say. (`None` cannot happen while `tx` was `Some`.)
            Some(Ok(Ok(()))) | None => {
                CliError::Usage(format!("{} writer stopped early", self.table))
            }
        };
        self.failure = Some(error.to_string());
        Err(error)
    }

    /// Ship the trailing partial batch, close the channel and surface the
    /// writer's own result. Any panic in the writer becomes an error here --
    /// it must never be mistaken for a completed file.
    pub(super) fn finish(mut self) -> Result<(), CliError> {
        let table = self.table;
        // An earlier `ship` already joined the stopped writer and returned its
        // error to that caller. The file is incomplete, so finishing can only
        // fail. Without this the batch that `ship` emptied before its failed
        // send made `finish` report success.
        if let Some(cause) = self.failure.take() {
            return Err(CliError::Usage(format!(
                "{table} writer had already failed: {cause}"
            )));
        }
        // A `ship` that finds the writer gone has joined it and returns the
        // writer's own error.
        if !self.batch.is_empty() {
            self.ship()?;
        }
        // Dropping the sender is what ends the writer loop.
        self.tx = None;
        match self.handle.take().map(thread::JoinHandle::join) {
            Some(Ok(result)) => result.map_err(CliError::Export),
            Some(Err(_)) => Err(CliError::Usage(format!("{table} writer thread panicked"))),
            // Only a failed `ship` joins early, and that returned above.
            // Never `Ok` regardless: there is no file.
            None => Err(CliError::Usage(format!(
                "{table} writer was joined before finishing"
            ))),
        }
    }
}

impl<T> Drop for WriterThread<T> {
    fn drop(&mut self) {
        // Every early return closes the producer side and waits for the writer
        // to observe cancellation. Dropping JoinHandle without joining would
        // detach a thread still writing into a staging directory that the
        // caller is about to remove or publish.
        self.tx = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, mpsc};
    use std::time::Duration;

    #[test]
    fn dropping_a_writer_closes_and_joins_its_thread() {
        let gate = Arc::new(Barrier::new(2));
        let writer_gate = Arc::clone(&gate);
        let writer = WriterThread::<u8>::spawn("test", move |rx| {
            for _ in rx {}
            writer_gate.wait();
            Ok(())
        });
        let (drop_done, dropped) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(writer);
            drop_done.send(()).unwrap();
        });

        assert!(
            dropped.recv_timeout(Duration::from_millis(50)).is_err(),
            "drop returned while its writer thread was still running"
        );
        gate.wait();
        dropped.recv_timeout(Duration::from_secs(1)).unwrap();
        dropper.join().unwrap();
    }

    /// A writer that fails must never be reported as a finished file.
    ///
    /// Moving the Parquet writers onto threads moved their errors off the `?`
    /// path, which is exactly the shape of a silent success. The case only
    /// `finish` can catch is a writer that drained its channel and then
    /// failed -- a disk full while writing the Parquet footer: no send fails,
    /// so the error exists only in the join. The test this replaced failed
    /// its writer mid-stream, which a send always noticed first, and it
    /// stayed green with that join arm made to swallow the error.
    #[test]
    fn a_writer_that_fails_after_draining_its_channel_fails_finish() {
        let mut writer = WriterThread::<u8>::spawn("test", |rx| {
            for _ in rx {}
            Err(ExportError::Usage("footer failed".into()))
        });
        writer.append(&mut vec![0u8; 3]).unwrap();
        let error = writer
            .finish()
            .expect_err("a writer whose footer failed must not finish");
        assert!(error.to_string().contains("footer failed"), "got: {error}");
    }

    /// Append full batches until one is refused. The channel holds
    /// `WRITER_QUEUE_DEPTH` batches, so once the writer has stopped reading,
    /// a send fails within that many more plus the one it took: bounded, not
    /// a race.
    fn append_until_refused(writer: &mut WriterThread<u8>) -> CliError {
        for _ in 0..(WRITER_QUEUE_DEPTH + 2) {
            let mut batch = vec![0u8; WRITER_BATCH_ROWS];
            if let Err(error) = writer.append(&mut batch) {
                return error;
            }
        }
        panic!("the writer stopped reading, so a send must have failed by now");
    }

    /// The error `append` returns is the one the export reports, so it must be
    /// the writer's own.
    ///
    /// The driver returns `append`'s error from the packet loop and never
    /// reaches `finish`, and `Drop` discards what it joins. `append` used to
    /// say only "writer stopped early", so a disk-full or Parquet encode
    /// failure mid-export printed that and nothing else: the one message an
    /// operator needs was thrown away. The test above never looked at
    /// `append`'s error at all.
    #[test]
    fn append_reports_the_writer_threads_own_error() {
        let mut writer = WriterThread::<u8>::spawn("test", |rx| {
            let _ = rx.recv();
            Err(ExportError::Usage("writer failed".into()))
        });
        let error = append_until_refused(&mut writer);
        assert!(
            matches!(error, CliError::Export(_)),
            "the writer's error keeps its class: {error:?}"
        );
        assert!(
            error.to_string().contains("writer failed"),
            "append must surface the writer's own error, got: {error}"
        );
        // The failure was already reported, and the handle joined: `finish`
        // must neither panic on the missing handle nor report a finished file.
        let error = writer
            .finish()
            .expect_err("a writer that failed must not finish successfully");
        assert!(
            error.to_string().contains("writer failed"),
            "finish must name the same cause, got: {error}"
        );
    }

    /// A panic found through `append` is named as a panic, not as an early
    /// stop. The panic message printed during `cargo test` is expected.
    #[test]
    fn append_reports_a_panicking_writer_as_a_panic() {
        let mut writer = WriterThread::<u8>::spawn("test", |rx| {
            let _ = rx.recv();
            panic!("writer died")
        });
        let error = append_until_refused(&mut writer);
        assert!(error.to_string().contains("panicked"), "got: {error}");
        assert!(writer.finish().is_err());
    }

    /// Only a writer that returned `Ok` without draining its channel has
    /// nothing more specific to say than that it stopped early.
    #[test]
    fn append_reports_a_writer_that_returned_ok_early_as_stopped() {
        let mut writer = WriterThread::<u8>::spawn("test", |rx| {
            let _ = rx.recv();
            Ok(())
        });
        let error = append_until_refused(&mut writer);
        assert!(error.to_string().contains("stopped early"), "got: {error}");
        assert!(writer.finish().is_err());
    }

    /// A panicking writer thread must also be an error. `JoinHandle::join`
    /// returns `Err` on panic and it would be easy to discard.
    ///
    /// The panic message this prints on stderr during `cargo test` is expected.
    #[test]
    fn a_panicking_writer_thread_is_reported_not_swallowed() {
        let writer = WriterThread::<u8>::spawn("test", |_rx| panic!("writer died"));
        let err = writer
            .finish()
            .expect_err("a panicking writer must not report success");
        assert!(
            err.to_string().contains("panicked"),
            "finish must name the panic, got: {err}"
        );
    }
}
