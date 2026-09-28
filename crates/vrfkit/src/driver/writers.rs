//! Parquet writers running off the packet loop.
//!
//! Inline in the packet loop, encoding `fields` and `movement` (Arrow batch
//! build + ZSTD) measured 570 ms and 450 ms of a 2.60 s export, 37% of the
//! run. Each table is an independent file whose writer reads no replay state,
//! so each runs on its own thread, fed over a bounded channel so a slow writer
//! applies backpressure. One consumer sees every record once, in stream
//! order, and row groups close on the same cumulative counts, so the bytes
//! are unchanged. `actors`, `net_guids` and `events` stay inline: together
//! under 1% of the write cost.
//!
//! No error is dropped. A failed writer returns its error and drops its
//! receiver, so the next send fails; `ship` then joins the thread and returns
//! the writer's own error, because the packet loop returns from there without
//! reaching `finish` and `Drop` discards what it joins. A panic is an error,
//! and a writer that failed never finishes `Ok`.

use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;

use vrf_export::ExportError;

use crate::error::CliError;

/// Rows gathered in the packet loop per message to a writer thread; one
/// message per packet would cost more in channel traffic than the encoding it
/// hides. Measured at 061155a on 02d4d478 (pyarrow row counts over its 530,401
/// packets): `fields` 1,296,660 rows, 2.44 per packet, about 80 batches;
/// `movement` 1,844,147 rows, 3.48 per packet, about 113.
const WRITER_BATCH_ROWS: usize = 16_384;

/// Batches in flight per writer, bounding memory: four field batches are about
/// 10 MB of records plus their raw-bit payloads. Names are interned `Arc<str>`,
/// so a queued batch holds refcounts, not strings.
const WRITER_QUEUE_DEPTH: usize = 4;

/// A writer running on its own thread, plus the handle needed to collect its
/// result. `T` is the record type of the table it owns.
pub(super) struct WriterThread<T> {
    tx: Option<SyncSender<Vec<T>>>,
    handle: Option<thread::JoinHandle<Result<(), ExportError>>>,
    batch: Vec<T>,
    /// Table name, used only to name the failing table in an error message.
    table: &'static str,
    /// Why the writer stopped, once `ship` joined it. The error went to that
    /// caller (it is not `Clone`); a later `finish` names the same cause.
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
        // A send fails only once the receiver is gone, so this join cannot
        // block, and it is the last place the writer's own error exists (see
        // the module doc).
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

    /// Ship the trailing partial batch, close the channel and return the
    /// writer's own result; a panic is an error, never a completed file.
    pub(super) fn finish(mut self) -> Result<(), CliError> {
        let table = self.table;
        // An earlier `ship` joined the stopped writer and returned its error;
        // the file is incomplete, so finishing can only fail.
        if let Some(cause) = self.failure.take() {
            return Err(CliError::Usage(format!(
                "{table} writer had already failed: {cause}"
            )));
        }
        // A failing `ship` has joined the writer and returns its own error.
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
        // Close the producer side and join: a detached thread could still be
        // writing into a staging directory the caller removes or publishes.
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

    /// The case only `finish` can catch: a writer that drained its channel and
    /// then failed (a disk full on the footer), so no send fails and the error
    /// exists only in the join. A mid-stream failure cannot test this arm: a
    /// send always sees it first.
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

    /// `append`'s error is the one the export reports (see the module doc).
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
        // Already reported and joined: `finish` must neither panic on the
        // missing handle nor report a finished file.
        let error = writer
            .finish()
            .expect_err("a writer that failed must not finish successfully");
        assert!(
            error.to_string().contains("writer failed"),
            "finish must name the same cause, got: {error}"
        );
    }

    /// The panic message `cargo test` prints here is expected.
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

    /// The panic message `cargo test` prints here is expected.
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
