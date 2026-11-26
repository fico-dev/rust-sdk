//! Following a local run's logs, `logs/logs.log` in its directory, while it
//! runs. No tool serves it: it hands each line to a callback as it arrives.

use std::io::{self, SeekFrom};
use std::path::Path;

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{Local, run_dir};
use crate::decision::polling::Poller;
use crate::decision::{LocalWorkspaceRef, PollingOptions};
use crate::{Error, Result};

/// Parameters for [`Local::follow_run_logs`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunLogsFollowParams {
    /// The waits between checks of the run, and the limits of the whole
    /// follow. With the defaults, following has no time or try limit.
    pub polling: PollingOptions,
}

impl Local {
    /// Follows the logs of a local run until it is final, handing each new
    /// line to `on_line` as it arrives, in order, on the calling task.
    /// Returns every line.
    ///
    /// Each check reads the run's status, as [`Local::get_run_information`]
    /// does, then what the run added to its logs since the last check. Only
    /// whole lines go out, without their line ending; a last line with no
    /// newline goes out once the run is final. The check after the run is
    /// final is the last one: by then, the run's logs are complete. A run
    /// that has written no logs yet has no lines. Workspaces may write
    /// anything to stderr, so bytes that are not UTF-8 are replaced.
    ///
    /// Following has no limit unless `max_duration` or `max_tries` is set,
    /// and a status this SDK does not know is not final, so following waits
    /// on it until a limit or the caller's own timeout stops it. Dropping
    /// the future stops following.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if the run does not exist,
    /// [`Error::Invalid`] if `run_id` cannot be a local run ID or a duration
    /// in `params.polling` is not valid, [`Error::Wait`] if following runs
    /// out of time or tries, and [`Error::Io`] if the run's files cannot be
    /// read. Finding the workspace fails as [`Local::get_workspace`] does,
    /// except that a `src` needs no registration.
    pub async fn follow_run_logs<F>(
        &self,
        workspace: &LocalWorkspaceRef,
        run_id: &str,
        params: RunLogsFollowParams,
        mut on_line: F,
    ) -> Result<Vec<String>>
    where
        F: FnMut(&str) + Send,
    {
        let (_, dir) = self.find_run(workspace, run_id).await?;
        let path = run_dir::logs_file(&dir);
        let mut poller = Poller::start(&params.polling).await?;
        let mut tail = Tail::default();
        let mut lines = Vec::new();
        let mut emit = |line: &[u8]| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = String::from_utf8_lossy(line).into_owned();
            on_line(&line);
            lines.push(line);
        };
        loop {
            // The status first: once it is final, the logs are complete.
            let information = self.get_run_information(workspace, run_id).await?;
            let done = information.metadata.status.is_terminal();
            tail.read(&path).await?;
            if let Some(whole) = tail.take_lines() {
                whole.split(|&byte| byte == b'\n').for_each(&mut emit);
            }
            if done {
                if !tail.pending.is_empty() {
                    emit(&tail.pending);
                }
                return Ok(lines);
            }
            poller.wait(None).await?;
        }
    }
}

/// What has been read of a log file that only grows.
#[derive(Debug, Default)]
struct Tail {
    /// Bytes read so far.
    offset: u64,
    /// Bytes read after the last newline taken.
    pending: Vec<u8>,
}

impl Tail {
    /// Reads what the file at `path` holds past `offset`. A file that does
    /// not exist holds nothing yet.
    async fn read(&mut self, path: &Path) -> Result<()> {
        let io_error = |source| Error::Io {
            path: path.to_owned(),
            source,
        };
        let mut file = match fs::File::open(path).await {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_error(source)),
        };
        file.seek(SeekFrom::Start(self.offset))
            .await
            .map_err(io_error)?;
        let read = file
            .read_to_end(&mut self.pending)
            .await
            .map_err(io_error)?;
        self.offset += read as u64;
        Ok(())
    }

    /// The whole lines read since the last call, without the last `\n`,
    /// or `None` if no line was completed.
    fn take_lines(&mut self) -> Option<Vec<u8>> {
        let end = self.pending.iter().rposition(|&byte| byte == b'\n')?;
        let rest = self.pending.split_off(end + 1);
        let mut whole = std::mem::replace(&mut self.pending, rest);
        whole.pop();
        Some(whole)
    }
}
