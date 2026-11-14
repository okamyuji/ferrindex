//! FSEvents の履歴を「目印」で区切って引く計測用の部品。
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use core_foundation::string::CFString;
use fsevent_sys as fs;
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::time::{Duration, Instant};

pub use fs::FSEventStreamEventFlags as Flags;

#[derive(Debug, Clone)]
pub struct Ev {
    pub path: String,
    pub flags: Flags,
    pub id: u64,
}

pub struct Query {
    pub events: Vec<Ev>,
    /// 目印のイベントID。次回の起点にしてよいのはこの値だけ
    pub sentinel_id: Option<u64>,
    pub elapsed: Duration,
}

struct Acc {
    events: Vec<Ev>,
}

extern "C" fn cb(
    _s: fs::FSEventStreamRef,
    info: *mut c_void,
    n: usize,
    paths: *mut c_void,
    flags: *const fs::FSEventStreamEventFlags,
    ids: *const fs::FSEventStreamEventId,
) {
    let acc = unsafe { &mut *(info as *mut Acc) };
    let paths = paths as *const *const c_char;
    for i in 0..n {
        let p = unsafe { CStr::from_ptr(*paths.add(i)) }.to_string_lossy().into_owned();
        acc.events.push(Ev { path: p, flags: unsafe { *flags.add(i) }, id: unsafe { *ids.add(i) } });
    }
}

pub fn current_id() -> u64 {
    unsafe { fs::FSEventsGetCurrentEventId() }
}

pub const DROPPED: Flags = fs::kFSEventStreamEventFlagMustScanSubDirs
    | fs::kFSEventStreamEventFlagUserDropped
    | fs::kFSEventStreamEventFlagKernelDropped
    | fs::kFSEventStreamEventFlagEventIdsWrapped
    | fs::kFSEventStreamEventFlagRootChanged;
pub const IS_DIR: Flags = fs::kFSEventStreamEventFlagItemIsDir;
pub const HISTORY_DONE: Flags = fs::kFSEventStreamEventFlagHistoryDone;

/// dirs を since より後から見張り、開始後に一意な目印を書き、その到着までの変更を返す。
pub fn query(dirs: &[&str], sentinel: &str, since: u64, timeout: Duration) -> Query {
    let t = Instant::now();
    let mut acc = Acc { events: vec![] };
    let ctx = fs::FSEventStreamContext {
        version: 0,
        info: &mut acc as *mut Acc as *mut c_void,
        retain: None,
        release: None,
        copy_description: None,
    };
    let cf: Vec<CFString> = dirs.iter().map(|d| CFString::new(d)).collect();
    let arr = CFArray::from_CFTypes(&cf);
    let flags = fs::kFSEventStreamCreateFlagFileEvents | fs::kFSEventStreamCreateFlagNoDefer;
    let mut sentinel_id = None;
    unsafe {
        let s = fs::FSEventStreamCreate(
            std::ptr::null_mut(), cb, &ctx, arr.as_concrete_TypeRef() as _, since, 0.0, flags,
        );
        let rl = CFRunLoop::get_current();
        fs::FSEventStreamScheduleWithRunLoop(s, rl.as_concrete_TypeRef() as _, kCFRunLoopDefaultMode as _);
        fs::FSEventStreamStart(s);
        std::fs::write(sentinel, b"s").expect("sentinel");
        let deadline = Instant::now() + timeout;
        let wait_history = std::env::var("WAIT_HISTORY").is_ok_and(|v| v == "1");
        let mut history_done = false;
        while Instant::now() < deadline && (sentinel_id.is_none() || (wait_history && !history_done)) {
            CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, Duration::from_millis(1), true);
            sentinel_id = acc.events.iter().find(|e| e.path == sentinel).map(|e| e.id);
            history_done = acc.events.iter().any(|e| e.flags & HISTORY_DONE != 0);
        }
        if std::env::var("FLUSH").is_ok_and(|v| v == "1") {
            fs::FSEventStreamFlushSync(s);
        }
        // 診断: 目印の後にさらに待ち、目印より小さいIDが遅れて届くかを数える
        let linger: u64 = std::env::var("LINGER_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        let n_at_sentinel = acc.events.len();
        let end = Instant::now() + Duration::from_millis(linger);
        while Instant::now() < end {
            CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, Duration::from_millis(1), true);
        }
        if let Some(sid) = sentinel_id {
            let late = acc.events[n_at_sentinel..].iter().filter(|e| e.id < sid).count();
            if late > 0 { eprintln!("late_events_below_sentinel={late}"); }
        }
        fs::FSEventStreamStop(s);
        fs::FSEventStreamInvalidate(s);
        fs::FSEventStreamRelease(s);
    }
    let _ = std::fs::remove_file(sentinel);
    Query { events: acc.events, sentinel_id, elapsed: t.elapsed() }
}
