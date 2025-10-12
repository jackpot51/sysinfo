// Take a look at the license at the top of the repository in the LICENSE file.

use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, DirEntry, File, read_dir};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::str::{self, FromStr};
use std::sync::atomic::{AtomicUsize, Ordering};

use libc::{c_ulong, gid_t, uid_t};

use crate::sys::system::SystemInfo;
use crate::sys::utils::{
    PathHandler, PathPush, get_all_data_from_file, get_all_utf8_data, realpath,
};
use crate::{
    DiskUsage, Gid, Pid, Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, Signal,
    ThreadKind, Uid,
};

use crate::sys::system::remaining_files;

#[doc(hidden)]
impl From<char> for ProcessStatus {
    fn from(status: char) -> ProcessStatus {
        match status {
            'R' => ProcessStatus::Run,
            'B' | 'S' => ProcessStatus::Sleep,
            'I' => ProcessStatus::Idle,
            'D' => ProcessStatus::UninterruptibleDiskSleep,
            'Z' => ProcessStatus::Zombie,
            'T' => ProcessStatus::Stop,
            't' => ProcessStatus::Tracing,
            'X' | 'x' => ProcessStatus::Dead,
            'K' => ProcessStatus::Wakekill,
            'W' => ProcessStatus::Waking,
            'P' => ProcessStatus::Parked,
            x => ProcessStatus::Unknown(x as u32),
        }
    }
}

impl fmt::Display for ProcessStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match *self {
            ProcessStatus::Idle => "Idle",
            ProcessStatus::Run => "Runnable",
            ProcessStatus::Sleep => "Sleeping",
            ProcessStatus::Stop => "Stopped",
            ProcessStatus::Zombie => "Zombie",
            ProcessStatus::Tracing => "Tracing",
            ProcessStatus::Dead => "Dead",
            ProcessStatus::Wakekill => "Wakekill",
            ProcessStatus::Waking => "Waking",
            ProcessStatus::Parked => "Parked",
            ProcessStatus::UninterruptibleDiskSleep => "UninterruptibleDiskSleep",
            _ => "Unknown",
        })
    }
}

#[allow(dead_code)]
#[repr(usize)]
enum ProcIndex {
    Pid = 0,
    State,
    ParentPid,
    GroupId,
    SessionId,
    Tty,
    ForegroundProcessGroupId,
    Flags,
    MinorFaults,
    ChildrenMinorFaults,
    MajorFaults,
    ChildrenMajorFaults,
    UserTime,
    SystemTime,
    ChildrenUserTime,
    ChildrenKernelTime,
    Priority,
    Nice,
    NumberOfThreads,
    IntervalTimerSigalarm,
    StartTime,
    VirtualSize,
    ResidentSetSize,
    // More exist but we only use the listed ones. For more, take a look at `man proc`.
}

pub(crate) struct ProcessInner {
    pub(crate) name: OsString,
    pub(crate) cmd: Vec<OsString>,
    pub(crate) exe: Option<PathBuf>,
    pub(crate) pid: Pid,
    parent: Option<Pid>,
    pub(crate) environ: Vec<OsString>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) root: Option<PathBuf>,
    pub(crate) memory: u64,
    pub(crate) virtual_memory: u64,
    utime: u64,
    stime: u64,
    old_utime: u64,
    old_stime: u64,
    start_time_without_boot_time: u64,
    start_time: u64,
    start_time_raw: u64,
    run_time: u64,
    pub(crate) updated: bool,
    cpu_usage: f32,
    user_id: Option<Uid>,
    effective_user_id: Option<Uid>,
    group_id: Option<Gid>,
    effective_group_id: Option<Gid>,
    pub(crate) status: ProcessStatus,
    pub(crate) tasks: Option<HashSet<Pid>>,
    stat_file: Option<FileCounter>,
    old_read_bytes: u64,
    old_written_bytes: u64,
    read_bytes: u64,
    written_bytes: u64,
    thread_kind: Option<ThreadKind>,
    proc_path: PathBuf,
    accumulated_cpu_time: u64,
    exists: bool,
}

impl ProcessInner {
    pub(crate) fn new(pid: Pid, proc_path: PathBuf) -> Self {
        Self {
            name: OsString::new(),
            pid,
            parent: None,
            cmd: Vec::new(),
            environ: Vec::new(),
            exe: None,
            cwd: None,
            root: None,
            memory: 0,
            virtual_memory: 0,
            cpu_usage: 0.,
            utime: 0,
            stime: 0,
            old_utime: 0,
            old_stime: 0,
            updated: true,
            start_time_without_boot_time: 0,
            start_time: 0,
            start_time_raw: 0,
            run_time: 0,
            user_id: None,
            effective_user_id: None,
            group_id: None,
            effective_group_id: None,
            status: ProcessStatus::Unknown(0),
            tasks: None,
            stat_file: None,
            old_read_bytes: 0,
            old_written_bytes: 0,
            read_bytes: 0,
            written_bytes: 0,
            thread_kind: None,
            proc_path,
            accumulated_cpu_time: 0,
            exists: true,
        }
    }

    pub(crate) fn kill_with(&self, signal: Signal) -> Option<bool> {
        let c_signal = crate::sys::system::convert_signal(signal)?;
        unsafe { Some(libc::kill(self.pid.0, c_signal) == 0) }
    }

    pub(crate) fn name(&self) -> &OsStr {
        &self.name
    }

    pub(crate) fn cmd(&self) -> &[OsString] {
        &self.cmd
    }

    pub(crate) fn exe(&self) -> Option<&Path> {
        self.exe.as_deref()
    }

    pub(crate) fn pid(&self) -> Pid {
        self.pid
    }

    pub(crate) fn environ(&self) -> &[OsString] {
        &self.environ
    }

    pub(crate) fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub(crate) fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub(crate) fn memory(&self) -> u64 {
        self.memory
    }

    pub(crate) fn virtual_memory(&self) -> u64 {
        self.virtual_memory
    }

    pub(crate) fn parent(&self) -> Option<Pid> {
        self.parent
    }

    pub(crate) fn status(&self) -> ProcessStatus {
        self.status
    }

    pub(crate) fn start_time(&self) -> u64 {
        self.start_time
    }

    pub(crate) fn run_time(&self) -> u64 {
        self.run_time
    }

    pub(crate) fn cpu_usage(&self) -> f32 {
        self.cpu_usage
    }

    pub(crate) fn accumulated_cpu_time(&self) -> u64 {
        self.accumulated_cpu_time
    }

    pub(crate) fn disk_usage(&self) -> DiskUsage {
        DiskUsage {
            written_bytes: self.written_bytes.saturating_sub(self.old_written_bytes),
            total_written_bytes: self.written_bytes,
            read_bytes: self.read_bytes.saturating_sub(self.old_read_bytes),
            total_read_bytes: self.read_bytes,
        }
    }

    pub(crate) fn user_id(&self) -> Option<&Uid> {
        self.user_id.as_ref()
    }

    pub(crate) fn effective_user_id(&self) -> Option<&Uid> {
        self.effective_user_id.as_ref()
    }

    pub(crate) fn group_id(&self) -> Option<Gid> {
        self.group_id
    }

    pub(crate) fn effective_group_id(&self) -> Option<Gid> {
        self.effective_group_id
    }

    pub(crate) fn wait(&self) -> Option<ExitStatus> {
        // If anything fails when trying to retrieve the start time, better to return `None`.
        let (data, _) = _get_stat_data_and_file(&self.proc_path).ok()?;
        let parts = parse_stat_file(&data)?;

        if start_time_raw(&parts) != self.start_time_raw {
            sysinfo_debug!("Seems to not be the same process anymore");
            return None;
        }

        crate::unix::utils::wait_process(self.pid)
    }

    pub(crate) fn session_id(&self) -> Option<Pid> {
        unsafe {
            unsafe extern "C" {
                //TODO: expose getsid in libc crate
                fn getsid(pid: libc::pid_t) -> libc::pid_t;
            }
            let session_id = getsid(self.pid.0);
            if session_id < 0 {
                None
            } else {
                Some(Pid(session_id))
            }
        }
    }

    pub(crate) fn thread_kind(&self) -> Option<ThreadKind> {
        self.thread_kind
    }

    pub(crate) fn switch_updated(&mut self) -> bool {
        std::mem::replace(&mut self.updated, false)
    }

    pub(crate) fn set_nonexistent(&mut self) {
        self.exists = false;
    }

    pub(crate) fn exists(&self) -> bool {
        self.exists
    }

    pub(crate) fn open_files(&self) -> Option<usize> {
        let open_files_dir = self.proc_path.as_path().join("fd");
        match fs::read_dir(&open_files_dir) {
            Ok(entries) => Some(entries.count() as _),
            Err(_error) => {
                sysinfo_debug!(
                    "Failed to get open files in `{}`: {_error:?}",
                    open_files_dir.display(),
                );
                None
            }
        }
    }

    pub(crate) fn open_files_limit(&self) -> Option<usize> {
        let limits_files = self.proc_path.as_path().join("limits");
        match fs::read_to_string(&limits_files) {
            Ok(content) => {
                for line in content.lines() {
                    if let Some(line) = line.strip_prefix("Max open files ")
                        && let Some(nb) = line.split_whitespace().find(|p| !p.is_empty())
                    {
                        return usize::from_str(nb).ok();
                    }
                }
                None
            }
            Err(_error) => {
                sysinfo_debug!(
                    "Failed to get limits in `{}`: {_error:?}",
                    limits_files.display()
                );
                None
            }
        }
    }
}

pub(crate) fn compute_cpu_usage(p: &mut ProcessInner, total_time: f32, max_value: f32) {
    // First time updating the values without reference, wait for a second cycle to update cpu_usage
    if p.old_utime == 0 && p.old_stime == 0 {
        return;
    }

    // We use `max_value` to ensure that the process CPU usage will never get bigger than:
    // `"number of CPUs" * 100.`
    p.cpu_usage = (p
        .utime
        .saturating_sub(p.old_utime)
        .saturating_add(p.stime.saturating_sub(p.old_stime)) as f32
        / total_time
        * 100.)
        .min(max_value);
}

pub(crate) fn set_time(p: &mut ProcessInner, utime: u64, stime: u64) {
    p.old_utime = p.utime;
    p.old_stime = p.stime;
    p.utime = utime;
    p.stime = stime;
}

#[inline(always)]
fn start_time_raw(parts: &Parts<'_>) -> u64 {
    u64::from_str(parts.str_parts[ProcIndex::StartTime as usize]).unwrap_or(0)
}

fn _get_stat_data_and_file(path: &Path) -> Result<(Vec<u8>, File), ()> {
    let mut file = File::open(path.join("stat")).map_err(|_| ())?;
    let data = get_all_data_from_file(&mut file, 1024).map_err(|_| ())?;
    Ok((data, file))
}

fn _get_stat_data(path: &Path, stat_file: &mut Option<FileCounter>) -> Result<Vec<u8>, ()> {
    let (data, file) = _get_stat_data_and_file(path)?;
    *stat_file = FileCounter::new(file);
    Ok(data)
}

#[inline(always)]
fn get_status(p: &mut ProcessInner, part: &str) {
    p.status = part
        .chars()
        .next()
        .map(ProcessStatus::from)
        .unwrap_or_else(|| ProcessStatus::Unknown(0));
}

/// We're forced to read the whole `/proc` folder because if a process died and another took its
/// place, we need to get the task parent (if it's a task).
pub(crate) fn refresh_procs(
    proc_list: &mut HashMap<Pid, Process>,
    proc_path: &Path,
    uptime: u64,
    info: &SystemInfo,
    processes_to_update: ProcessesToUpdate<'_>,
    refresh_kind: ProcessRefreshKind,
) -> usize {
 /* Example data from /scheme/proc/ps:
PID   PGID  PPID  SID   RUID  RGID  RNS   EUID  EGID  ENS   NTHRD STATUS  NAME
1     1     1     1     0     0     1     0     0     1     1     R       /scheme/initfs/bin/init
4     1     1     1     0     0     0     0     0     0     1     R       /bin/nulld
*/

/* Example data from /scheme/sys/context:
PID   EUID  EGID  ENS   STAT  CPU   AFFINITY   TIME        MEM     NAME
0     0     0     0     RR+   #3               00:00:01.36 1 KB    [kmain]
0     0     0     0     RR+   #2               00:00:01.35 1 KB    [kmain]
0     0     0     0     RR    #1               00:00:01.34 1 KB    [kmain]
0     0     0     0     RR+   #0               00:00:01.31 1 KB    [kmain]
0     0     0     1     UB    #3               00:00:00.00 23 MB   [init]
0     0     0     1     UB    #1               00:00:00.00 23 MB   [init]
1     0     0     1     UB    #3               00:00:00.01 1 MB    /scheme/initfs/bin/init
0     6     12    18    24    30    36         47 50 53 56 59      67
Indexes listed above
*/
    let mut nb_updated = 0;
    for line in fs::read_to_string(proc_path).unwrap_or_default().lines().skip(1) {
        let Ok(pid) = line[0..6].trim().parse::<usize>().map(Pid::from) else { continue };

        match processes_to_update {
            ProcessesToUpdate::All => {},
            ProcessesToUpdate::Some(pids) => if !pids.contains(&pid) {
                continue;
            }
        }

        let euid = Uid(line[6..12].trim().parse::<libc::uid_t>().unwrap_or_default());
        let egid = Gid(line[12..18].trim().parse::<libc::gid_t>().unwrap_or_default());
        //TODO: use ens?
        let mut stat = line[24..30].trim().chars();
        let kind = stat.next().unwrap_or_default();
        let status = stat.next().unwrap_or_default();
        //TODO: this ID may not map to the CPUs detected from /scheme/sys/cpu
        let cpu = line[31..36].trim().parse::<usize>().unwrap_or_default();
        //TODO: use affinity?
        let time =
            // Hours
            line[47..49].parse::<u64>().unwrap_or_default() * 3600 * 1000 + 
            // Minutes
            line[50..52].parse::<u64>().unwrap_or_default() * 60 * 1000 +
            // Seconds
            line[53..55].parse::<u64>().unwrap_or_default() * 1000 +
            // Centiseconds
            line[56..58].parse::<u64>().unwrap_or_default() * 10;
        let mut parts = line[59..67].trim().split(' ');
        let mut mem = parts.next().unwrap_or_default().parse::<u64>().unwrap_or_default();
        match parts.next().unwrap_or_default() {
            "B" => {},
            "KB" => mem *= 1024,
            "MB" => mem *= 1024 * 1024,
            "GB" => mem *= 1024 * 1024 * 1024,
            suffix => {
                sysinfo_debug!("unknown memory suffix {:?}", suffix);
            }
        }
        let name = &line[67..];

        //TODO: use TID or fill in tasks?
        //TODO: /proc not implemented so this path is not useful
        //TODO: fill in more fields
        let mut proc = proc_list.entry(pid).or_insert_with(|| Process {
            inner: ProcessInner::new(pid, Path::new("/proc").join(format!("{}", pid)))
        });
        let mut p = &mut proc.inner;
        p.name = name.into();
        p.memory = mem;
        p.virtual_memory = mem;
        //TODO: get real uid from /scheme/proc/ps
        p.user_id = Some(euid.clone());
        p.effective_user_id = Some(euid);
        //TODO: get real gid from /scheme/proc/ps
        p.group_id = Some(egid.clone());
        p.effective_group_id = Some(egid);
        p.status = ProcessStatus::from(status);
        p.thread_kind = Some(match kind {
            'U' => ThreadKind::Userland,
            _ => ThreadKind::Kernel,
        });
        //TODO: system time
        set_time(p, time, 0);

        nb_updated += 1;
    }
    nb_updated
}

struct Parts<'a> {
    str_parts: Vec<&'a str>,
    short_exe: &'a [u8],
}

fn parse_stat_file(data: &[u8]) -> Option<Parts<'_>> {
    // The stat file is "interesting" to parse, because spaces cannot
    // be used as delimiters. The second field stores the command name
    // surrounded by parentheses. Unfortunately, whitespace and
    // parentheses are legal parts of the command, so parsing has to
    // proceed like this: The first field is delimited by the first
    // whitespace, the second field is everything until the last ')'
    // in the entire string. All other fields are delimited by
    // whitespace.

    let mut str_parts = Vec::with_capacity(51);
    let mut data_it = data.splitn(2, |&b| b == b' ');
    str_parts.push(str::from_utf8(data_it.next()?).ok()?);
    let mut data_it = data_it.next()?.rsplitn(2, |&b| b == b')');
    let data = str::from_utf8(data_it.next()?).ok()?;
    let short_exe = data_it.next()?;
    str_parts.extend(data.split_whitespace());
    Some(Parts {
        str_parts,
        short_exe: short_exe.strip_prefix(b"(").unwrap_or(short_exe),
    })
}

/// Type used to correctly handle the `REMAINING_FILES` global.
struct FileCounter(File);

impl FileCounter {
    fn new(f: File) -> Option<Self> {
        let any_remaining =
            remaining_files().fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                if remaining > 0 {
                    Some(remaining - 1)
                } else {
                    // All file descriptors we were allowed are being used.
                    None
                }
            });

        any_remaining.ok().map(|_| Self(f))
    }
}

impl std::ops::Deref for FileCounter {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for FileCounter {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for FileCounter {
    fn drop(&mut self) {
        remaining_files().fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::split_content;
    use std::ffi::OsString;

    // This test ensures that all the parts of the data are split.
    #[test]
    fn test_copy_file() {
        assert_eq!(split_content(b"hello\0"), vec![OsString::from("hello")]);
        assert_eq!(split_content(b"hello"), vec![OsString::from("hello")]);
        assert_eq!(
            split_content(b"hello\0b"),
            vec![OsString::from("hello"), "b".into()]
        );
        assert_eq!(
            split_content(b"hello\0\0\0\0b"),
            vec![OsString::from("hello"), "b".into()]
        );
    }
}
